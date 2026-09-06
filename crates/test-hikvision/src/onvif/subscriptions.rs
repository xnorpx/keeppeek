use std::net::SocketAddr;
use std::time::Duration;

use super::soap::{self, EVENTS, Element, SOAP, VENDOR, WSNT};
use super::{Builder, PULL_WAIT_MAX, Shared, Subscription, clock};
use crate::Reply;

pub(super) fn create(operation: &Element, shared: &Shared) -> Reply {
    let lease = if operation
        .children
        .iter()
        .any(|child| child.is(EVENTS, "InitialTerminationTime"))
    {
        match duration_field(operation, EVENTS, "InitialTerminationTime") {
            Ok(lease) if !lease.is_zero() => lease.min(shared.config.lease),
            _ => {
                return soap::fault(
                    "wsnt:UnacceptableInitialTerminationTime",
                    "Invalid initial lease",
                    "",
                );
            }
        }
    } else {
        shared.config.lease
    };
    let mut state = shared.state.lock().unwrap();
    let now = shared.started.elapsed();
    if state
        .subscription
        .is_some_and(|subscription| subscription.expires > now)
    {
        return soap::fault(
            "wsnt:SubscribeCreationFailed",
            "Only one pull point is available",
            "",
        );
    }
    if state.stopped {
        return unknown();
    }
    let id = state
        .subscriptions
        .checked_add(1)
        .expect("fake ONVIF subscription identifiers exhausted");
    let Ok(address) = address(&shared.config, shared.address, id) else {
        return soap::fault(
            "ter:InvalidArgVal",
            "Invalid fixture subscription address",
            "",
        );
    };
    state.subscriptions = id;
    let expires = now + lease;
    state.subscription = Some(Subscription { id, expires });
    drop(state);
    shared.changed.notify_all();
    let address = xml::escape::escape_str_pcdata(&address);
    let current = clock::timestamp(now);
    let termination = clock::timestamp(expires);
    soap::reply(&format!(
        r#"<tev:CreatePullPointSubscriptionResponse><tev:SubscriptionReference>
        <wsa:Address>{address}</wsa:Address>
        <wsa:ReferenceParameters><v:Identifier wsa:IsReferenceParameter="true">{id}</v:Identifier></wsa:ReferenceParameters>
        </tev:SubscriptionReference><wsnt:CurrentTime>{current}</wsnt:CurrentTime>
        <wsnt:TerminationTime>{termination}</wsnt:TerminationTime></tev:CreatePullPointSubscriptionResponse>"#
    ))
}

pub(super) fn route(
    document: &Element,
    operation: &Element,
    target: &str,
    shared: &Shared,
) -> Reply {
    let Some(id) = subscription_id(target, shared) else {
        return unknown();
    };
    if !valid_reference(document, id) {
        return unknown();
    }
    if operation.is(EVENTS, "PullMessages") {
        return pull(operation, id, shared);
    }
    let mut state = shared.state.lock().unwrap();
    let now = shared.started.elapsed();
    if state.stopped || state.active(id, now).is_none() {
        return unknown();
    }
    let response = if operation.is(EVENTS, "SetSynchronizationPoint") {
        soap::reply("<tev:SetSynchronizationPointResponse/>")
    } else if operation.is(WSNT, "Renew") {
        let Ok(lease) = duration_field(operation, WSNT, "TerminationTime") else {
            return soap::fault(
                "wsnt:UnacceptableTerminationTime",
                "Invalid renewal lease",
                "",
            );
        };
        if lease.is_zero() {
            return soap::fault(
                "wsnt:UnacceptableTerminationTime",
                "Invalid renewal lease",
                "",
            );
        }
        let expires = now + lease.min(shared.config.lease);
        state.subscription = Some(Subscription { id, expires });
        state.renews += 1;
        soap::reply(&format!(
            "<wsnt:RenewResponse><wsnt:TerminationTime>{}</wsnt:TerminationTime><wsnt:CurrentTime>{}</wsnt:CurrentTime></wsnt:RenewResponse>",
            clock::timestamp(expires),
            clock::timestamp(now)
        ))
    } else if operation.is(WSNT, "Unsubscribe") {
        state.subscription = None;
        state.unsubscribes += 1;
        soap::reply("<wsnt:UnsubscribeResponse/>")
    } else {
        soap::fault(
            "ter:ActionNotSupported",
            "Unsupported subscription operation",
            "",
        )
    };
    shared.changed.notify_all();
    response
}

fn subscription_id(target: &str, shared: &Shared) -> Option<u64> {
    let address = url::Url::parse(&format!("http://{}{target}", shared.address)).ok()?;
    if address.path() != "/onvif/subscription" || address.fragment().is_some() {
        return None;
    }
    let mut query = address.query_pairs();
    let (name, value) = query.next()?;
    if name != "key" || query.next().is_some() {
        return None;
    }
    value.parse().ok()
}

fn valid_reference(document: &Element, id: u64) -> bool {
    document
        .child(SOAP, "Header")
        .and_then(|header| header.child(VENDOR, "Identifier"))
        .is_ok_and(|identifier| identifier.children.is_empty() && identifier.text == id.to_string())
}

fn duration_field(operation: &Element, namespace: &str, name: &str) -> anyhow::Result<Duration> {
    let value = operation.child(namespace, name)?;
    anyhow::ensure!(
        value.children.is_empty(),
        "invalid fake ONVIF duration element"
    );
    clock::parse_duration(value.text.trim())
        .ok_or_else(|| anyhow::anyhow!("invalid fake ONVIF duration"))
}

fn pull(operation: &Element, id: u64, shared: &Shared) -> Reply {
    let mut state = shared.state.lock().unwrap();
    let now = shared.started.elapsed();
    if state.stopped || state.active(id, now).is_none() {
        return unknown();
    }
    state.pulls += 1;
    shared.changed.notify_all();
    let Ok((timeout, limit)) = pull_parameters(operation, shared) else {
        return limit_fault(shared);
    };
    let deadline = now + timeout.min(PULL_WAIT_MAX);
    loop {
        let now = shared.started.elapsed();
        let Some(subscription) = state.active(id, now) else {
            return unknown();
        };
        if state.stopped {
            return unknown();
        }
        if let Some(reply) = state.pull_responses.pop_front() {
            return reply;
        }
        if !state.notifications.is_empty() || now >= deadline {
            let count = state.notifications.len().min(limit);
            let messages: Vec<_> = state.notifications.drain(..count).collect();
            drop(state);
            return pull_response(now, subscription.expires, &messages.concat());
        }
        let wait = deadline.min(subscription.expires).saturating_sub(now);
        (state, _) = shared.changed.wait_timeout(state, wait).unwrap();
    }
}

fn pull_parameters(operation: &Element, shared: &Shared) -> anyhow::Result<(Duration, usize)> {
    let timeout = duration_field(operation, EVENTS, "Timeout")?;
    let message_limit = operation.child(EVENTS, "MessageLimit")?;
    anyhow::ensure!(
        message_limit.children.is_empty(),
        "invalid fake ONVIF message limit element"
    );
    let limit = message_limit.text.trim().parse::<u32>()?;
    anyhow::ensure!(
        timeout <= shared.config.max_timeout,
        "fake ONVIF maximum timeout exceeded"
    );
    anyhow::ensure!(
        (1..=shared.config.max_message_limit).contains(&limit),
        "fake ONVIF message limit exceeded"
    );
    Ok((timeout, usize::try_from(limit)?))
}

fn pull_response(now: Duration, expires: Duration, messages: &str) -> Reply {
    soap::reply(&format!(
        "<tev:PullMessagesResponse><tev:CurrentTime>{}</tev:CurrentTime><tev:TerminationTime>{}</tev:TerminationTime>{messages}</tev:PullMessagesResponse>",
        clock::timestamp(now),
        clock::timestamp(expires)
    ))
}

fn limit_fault(shared: &Shared) -> Reply {
    soap::fault(
        "ter:InvalidArgVal",
        "Pull limits exceeded or invalid",
        &format!(
            "<tev:PullMessagesFaultResponse><tev:MaxTimeout>{}</tev:MaxTimeout><tev:MaxMessageLimit>{}</tev:MaxMessageLimit></tev:PullMessagesFaultResponse>",
            clock::duration_text(shared.config.max_timeout),
            shared.config.max_message_limit
        ),
    )
}

fn unknown() -> Reply {
    soap::fault(
        "wsrf-r:ResourceUnknown",
        "Unknown or expired subscription",
        "<wsrf-r:ResourceUnknownFault/>",
    )
}

pub(super) fn address(config: &Builder, address: SocketAddr, id: u64) -> anyhow::Result<String> {
    let template = config
        .subscription_address
        .as_deref()
        .unwrap_or("{origin}/onvif/subscription?key={id}");
    anyhow::ensure!(
        template.len() <= 1024 && !template.chars().any(char::is_control),
        "invalid fake ONVIF address template"
    );
    let expanded = template
        .replace("{origin}", &format!("http://{address}"))
        .replace("{port}", &address.port().to_string())
        .replace("{id}", &id.to_string());
    anyhow::ensure!(
        expanded.len() <= 4096,
        "fake ONVIF subscription address too long"
    );
    let parsed = url::Url::parse(&expanded)?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https")
            && parsed.host().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none(),
        "invalid fake ONVIF subscription address"
    );
    Ok(expanded)
}

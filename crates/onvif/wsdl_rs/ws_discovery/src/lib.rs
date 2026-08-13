pub mod probe {
    use yaserde_derive::YaSerialize;

    #[derive(Default, Eq, PartialEq, Debug, YaSerialize)]
    #[yaserde(
        prefix = "d",
        namespaces = { "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery" }
    )]
    pub struct Probe {
        #[yaserde(prefix = "d", rename = "Types")]
        pub types: String,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaSerialize)]
    #[yaserde(
        prefix = "s",
        namespaces = {
            "s" = "http://www.w3.org/2003/05/soap-envelope",
            "w" = "http://schemas.xmlsoap.org/ws/2004/08/addressing"
        }
    )]
    pub struct Header {
        #[yaserde(prefix = "w", rename = "MessageID")]
        pub message_id: String,

        #[yaserde(prefix = "w", rename = "To")]
        pub to: String,

        #[yaserde(prefix = "w", rename = "Action")]
        pub action: String,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaSerialize)]
    #[yaserde(
        prefix = "s",
        namespaces = {
            "s" = "http://www.w3.org/2003/05/soap-envelope",
            "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery"
        }
    )]
    pub struct Body {
        #[yaserde(prefix = "d", rename = "Probe")]
        pub probe: Probe,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaSerialize)]
    #[yaserde(
        prefix = "s",
        namespaces = {
            "s" = "http://www.w3.org/2003/05/soap-envelope",
            "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery",
            "w" = "http://schemas.xmlsoap.org/ws/2004/08/addressing"
        }
    )]
    pub struct Envelope {
        #[yaserde(prefix = "s", rename = "Header")]
        pub header: Header,

        #[yaserde(prefix = "s", rename = "Body")]
        pub body: Body,
    }
}

pub mod endpoint_reference {
    use yaserde_derive::YaDeserialize;

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(
        prefix = "wsa",
        namespaces = { "wsa" = "http://schemas.xmlsoap.org/ws/2004/08/addressing" }
    )]
    pub struct EndpointReference {
        #[yaserde(prefix = "wsa", rename = "Address")]
        pub address: String,
    }
}

pub mod probe_matches {
    use crate::endpoint_reference::EndpointReference;
    use percent_encoding::percent_decode_str;
    use url::Url;
    use yaserde_derive::YaDeserialize;

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(
        prefix = "d",
        namespaces = {
            "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery",
            "wsa" = "http://schemas.xmlsoap.org/ws/2004/08/addressing"
        }
    )]
    pub struct ProbeMatch {
        #[yaserde(prefix = "wsa", rename = "EndpointReference")]
        pub endpoint_reference: Option<EndpointReference>,

        #[yaserde(prefix = "d", rename = "Types")]
        pub types: Option<String>,

        #[yaserde(prefix = "d", rename = "Scopes")]
        pub scopes: Option<String>,

        #[yaserde(prefix = "d", rename = "XAddrs")]
        pub x_addrs: Option<String>,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(
        prefix = "d",
        namespaces = { "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery" }
    )]
    pub struct ProbeMatches {
        #[yaserde(prefix = "d", rename = "ProbeMatch")]
        pub probe_match: Vec<ProbeMatch>,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(
        prefix = "s",
        namespaces = {
            "s" = "http://www.w3.org/2003/05/soap-envelope",
            "w" = "http://schemas.xmlsoap.org/ws/2004/08/addressing"
        }
    )]
    pub struct Header {
        #[yaserde(prefix = "w", rename = "RelatesTo")]
        pub relates_to: String,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(
        prefix = "s",
        namespaces = {
            "s" = "http://www.w3.org/2003/05/soap-envelope",
            "d" = "http://schemas.xmlsoap.org/ws/2005/04/discovery"
        }
    )]
    pub struct Body {
        #[yaserde(prefix = "d", rename = "ProbeMatches")]
        pub probe_matches: ProbeMatches,
    }

    #[derive(Default, Eq, PartialEq, Debug, YaDeserialize)]
    #[yaserde(prefix = "s", namespaces = { "s" = "http://www.w3.org/2003/05/soap-envelope" })]
    pub struct Envelope {
        #[yaserde(prefix = "s", rename = "Header")]
        pub header: Header,

        #[yaserde(prefix = "s", rename = "Body")]
        pub body: Body,
    }

    impl ProbeMatch {
        pub fn types(&self) -> Vec<&str> {
            self.types
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .map(|t: &str| {
                    // Remove WSDL prefixes
                    t.find(':').map_or(t, |idx| t.split_at(idx + 1).1)
                })
                .collect()
        }

        pub fn scopes(&self) -> Vec<Url> {
            Self::split_string_to_urls(self.scopes.as_deref().unwrap_or(""))
        }

        pub fn x_addrs(&self) -> Vec<Url> {
            Self::split_string_to_urls(self.x_addrs.as_deref().unwrap_or(""))
        }

        pub fn name(&self) -> Option<String> {
            self.find_in_scopes("onvif://www.onvif.org/name/")
        }

        pub fn hardware(&self) -> Option<String> {
            self.find_in_scopes("onvif://www.onvif.org/hardware/")
        }

        pub fn endpoint_reference_address(&self) -> String {
            self.endpoint_reference
                .as_ref()
                .map(|e| e.address.to_string())
                .unwrap_or_default()
        }

        pub fn find_in_scopes(&self, prefix: &str) -> Option<String> {
            self.scopes().iter().find_map(|url| {
                url.as_str()
                    .strip_prefix(prefix)
                    .map(|s| percent_decode_str(s).decode_utf8_lossy().to_string())
            })
        }

        fn split_string_to_urls(s: &str) -> Vec<Url> {
            s.split_whitespace()
                .filter_map(|addr| Url::parse(addr).ok())
                .collect()
        }
    }

    #[test]
    fn probe_match() {
        let ser = r#"<?xml version="1.0" encoding="utf-8"?>
        <wsd:ProbeMatch xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery"
                        xmlns:dn="http://www.onvif.org/ver10/network/wsdl"
                        xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
            <wsd:Types>
                dn:NetworkVideoTransmitter
                tds:Device
            </wsd:Types>
            <wsd:Scopes>
                onvif://www.onvif.org/name/My%20Camera%202000
                onvif://www.onvif.org/hardware/My-HW-2000
                onvif://www.onvif.org/type/audio_encoder
                onvif://www.onvif.org/type/video_encoder
                onvif://www.onvif.org/type/ptz
                onvif://www.onvif.org/Profile/G
                onvif://www.onvif.org/Profile/Streaming
            </wsd:Scopes>
            <wsd:XAddrs>
                http://192.168.0.100:80/onvif/device_service
                http://10.0.0.200:80/onvif/device_service
            </wsd:XAddrs>
        </wsd:ProbeMatch>
        "#;

        let de: ProbeMatch = yaserde::de::from_str(ser).unwrap();

        assert_eq!(de.name(), Some("My Camera 2000".to_string()));
        assert_eq!(de.hardware(), Some("My-HW-2000".to_string()));
        assert!(
            de.find_in_scopes("onvif://www.onvif.org/type/video_encoder")
                .is_some()
        );
        assert!(
            de.find_in_scopes("onvif://www.onvif.org/type/video_analytics")
                .is_none()
        );
        assert_eq!(
            de.x_addrs(),
            vec![
                Url::parse("http://192.168.0.100:80/onvif/device_service").unwrap(),
                Url::parse("http://10.0.0.200:80/onvif/device_service").unwrap(),
            ]
        );

        assert_eq!(de.types(), vec!["NetworkVideoTransmitter", "Device"]);
    }
}

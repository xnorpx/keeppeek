//! Counts native SQL compilations and program starts inside an explicit measurement scope.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tracing_subscriber::{Layer, filter::filter_fn, layer::Context};

#[derive(Default)]
struct Totals {
    compilations: AtomicU64,
    executions: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(super) struct Counts {
    pub compilations: u64,
    pub executions: u64,
}

#[derive(Clone, Default)]
pub(super) struct Counter(Arc<Totals>);

impl Counter {
    pub(super) fn layer<S>(&self) -> impl Layer<S>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        self.clone().with_filter(filter_fn(|metadata| {
            metadata.name() == "retention_sql_measured"
                || metadata.name() == "translate"
                || metadata.name() == "trace_insn"
                || (*metadata.level() == tracing::Level::TRACE && metadata.fields().is_empty())
        }))
    }

    fn snapshot(&self) -> Counts {
        Counts {
            compilations: self.0.compilations.load(Ordering::Relaxed),
            executions: self.0.executions.load(Ordering::Relaxed),
        }
    }

    pub(super) fn measure<T>(&self, operation: impl FnOnce() -> T) -> (T, Counts) {
        let before = self.snapshot();
        let span = tracing::debug_span!("retention_sql_measured");
        let entered = span.enter();
        let result = operation();
        drop(entered);
        let after = self.snapshot();
        (
            result,
            Counts {
                compilations: after.compilations - before.compilations,
                executions: after.executions - before.executions,
            },
        )
    }
}

#[derive(Default)]
struct AddressZero(bool);
impl tracing::field::Visit for AddressZero {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "addr" {
            self.0 = value == 0;
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "addr" {
            self.0 = format!("{value:?}") == "0";
        }
    }
}

impl<S> Layer<S> for Counter
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        context: Context<'_, S>,
    ) {
        let metadata = attributes.metadata();
        if !context.lookup_current().is_some_and(|current| {
            current
                .scope()
                .take(32)
                .any(|span| span.name() == "retention_sql_measured")
        }) {
            return;
        }
        if metadata.name() == "translate" && metadata.target() == "turso_core::translate" {
            self.0.compilations.fetch_add(1, Ordering::Relaxed);
        }
        if metadata.name() == "trace_insn" && metadata.target() == "turso_core::vdbe" {
            // ponytail: Count instruction zero; ignore all SQL, instructions and parameter values.
            let mut address = AddressZero::default();
            attributes.record(&mut address);
            if address.0 {
                self.0.executions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn native_counter_counts_internal_schema_work_reused_statements_and_excludes_other_scopes() {
        let counter = Counter::default();
        let subscriber = tracing_subscriber::registry().with(counter.layer());
        tracing::subscriber::with_default(subscriber, || {
            let database =
                pollster::block_on(turso::Builder::new_local(":memory:").build()).unwrap();
            let connection = database.connect().unwrap();
            let (result, count) = counter.measure(|| {
                pollster::block_on(async {
                    connection
                        .execute_batch(
                            "CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1)",
                        )
                        .await?;
                    let mut rows = connection.query("SELECT value FROM sample", ()).await?;
                    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
                    drop(rows);
                    connection
                        .execute_batch("BEGIN; UPDATE sample SET value=2; COMMIT")
                        .await?;
                    anyhow::Ok(())
                })
            });
            result.unwrap();
            // Turso 0.7.2 also reads sqlite_schema after CREATE TABLE.
            assert_eq!(
                count,
                Counts {
                    compilations: 7,
                    executions: 7
                }
            );
            let (result, reused) = counter.measure(|| {
                pollster::block_on(async {
                    let mut statement = connection.prepare("SELECT value FROM sample").await?;
                    for _ in 0..3 {
                        let mut rows = statement.query(()).await?;
                        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 2);
                        assert!(rows.next().await?.is_none());
                    }
                    anyhow::Ok(())
                })
            });
            result.unwrap();
            assert_eq!(
                reused,
                Counts {
                    compilations: 1,
                    executions: 3
                }
            );
            let before = counter.snapshot();
            pollster::block_on(connection.execute("UPDATE sample SET value=3", ())).unwrap();
            assert_eq!(
                counter.snapshot(),
                before,
                "SQL outside measurement must be excluded"
            );
        });
    }
}

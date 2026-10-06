use anyhow::Result;

async fn initialize_columns(connection: &turso::Connection) -> Result<()> {
    for (column, declaration) in [
        (
            "retention_pending",
            "INTEGER NOT NULL DEFAULT 0 CHECK(retention_pending IN (0,1,2))",
        ),
        ("retention_generation", "INTEGER NOT NULL DEFAULT 0"),
        (
            "cleanup_retention_expiry",
            "INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_retention_expiry IN(0,1))",
        ),
        (
            "retention_error",
            "TEXT CHECK(length(retention_error)<=256)",
        ),
    ] {
        super::super::super::ensure_column(connection, "recording_files", column, declaration)
            .await?;
    }
    for (column, declaration) in [
        ("runtime_generation", "INTEGER NOT NULL DEFAULT 0"),
        (
            "expiry_eligible",
            "INTEGER NOT NULL DEFAULT 0 CHECK(expiry_eligible IN(0,1))",
        ),
        (
            "expiry_at_ms",
            "INTEGER NOT NULL DEFAULT -9223372036854775808",
        ),
    ] {
        super::super::super::ensure_column(
            connection,
            "recording_retention_decisions",
            column,
            declaration,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn initialize(connection: &turso::Connection) -> Result<()> {
    initialize_columns(connection).await?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS recording_retention_runtime (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>=0),
        settings_json TEXT, requested_json TEXT,
        request_pending INTEGER NOT NULL CHECK(request_pending IN(0,1)),
        complete INTEGER NOT NULL CHECK(complete IN(0,1)), cursor TEXT, high_water TEXT,
        work_clock INTEGER NOT NULL DEFAULT 0 CHECK(typeof(work_clock)='integer' AND work_clock>=0),
        overflow INTEGER NOT NULL DEFAULT 0 CHECK(overflow IN(0,1)),
        overflow_next INTEGER NOT NULL DEFAULT 0 CHECK(overflow_next IN(0,1)),
        spans_ready INTEGER NOT NULL DEFAULT 0 CHECK(spans_ready IN(0,1))
        ,expiry_cursor_ms INTEGER NOT NULL DEFAULT -9223372036854775808,
        expiry_cursor_id TEXT NOT NULL DEFAULT ''
    );
    INSERT OR IGNORE INTO recording_retention_runtime(singleton,generation,settings_json,requested_json,request_pending,complete,cursor,high_water)
        VALUES(1,0,NULL,NULL,0,1,NULL,NULL);
    CREATE INDEX IF NOT EXISTS recording_retention_pending_files ON recording_files(retention_pending,id);
    CREATE INDEX IF NOT EXISTS recording_retention_pending_generation ON recording_files(retention_pending,retention_generation,id);
    CREATE INDEX IF NOT EXISTS recording_retention_runtime_deadline
        ON recording_retention_decisions(runtime_generation,expiry_eligible,expiry_at_ms,recording_id);
    CREATE INDEX IF NOT EXISTS recording_retention_camera_files ON recording_files(source_id,started_at_ms,id);
    CREATE INDEX IF NOT EXISTS recording_retention_legacy_claims ON recording_files(id) WHERE cleanup_pending=1;
    CREATE INDEX IF NOT EXISTS recording_retention_named_claims ON storage_recording_retirements(operation) WHERE complete=0;
    CREATE TABLE IF NOT EXISTS recording_retention_policies (
        camera_id TEXT PRIMARY KEY, policy_json TEXT NOT NULL CHECK(length(policy_json)<=8192),
        uses_events INTEGER NOT NULL CHECK(uses_events IN(0,1))
    );
    CREATE TABLE IF NOT EXISTS recording_retention_camera_spans (
        camera_id TEXT PRIMARY KEY, span_ms INTEGER NOT NULL CHECK(typeof(span_ms)='integer' AND span_ms>0)
    );
    CREATE TABLE IF NOT EXISTS recording_retention_camera_work (
        camera_id TEXT PRIMARY KEY, lower_ms INTEGER NOT NULL, upper_ms INTEGER NOT NULL,
        cursor_ms INTEGER, cursor_id TEXT, next_lower_ms INTEGER, next_upper_ms INTEGER,
        high_water_ms INTEGER, high_water_id TEXT,last_processed INTEGER NOT NULL DEFAULT 0,
        generation INTEGER NOT NULL,queued_lower_ms INTEGER,queued_upper_ms INTEGER
    );").await?;
    recording_triggers(connection).await?;
    connection.execute_batch("CREATE TRIGGER IF NOT EXISTS recording_retention_legacy_hold_fence
        BEFORE UPDATE OF protected ON recording_files WHEN OLD.cleanup_pending=1 AND NEW.protected!=OLD.protected
        BEGIN SELECT RAISE(ABORT,'automatic cleanup owns this recording'); END;").await?;
    decision_triggers(connection).await?;
    sync_event_triggers(connection).await?;
    Ok(())
}

async fn decision_triggers(connection: &turso::Connection) -> Result<()> {
    for (name, event) in [("insert", "INSERT"), ("update", "UPDATE OF deadline_ms")] {
        connection.execute_batch(format!("CREATE TRIGGER IF NOT EXISTS recording_retention_runtime_deadline_{name}
            AFTER {event} ON recording_retention_decisions BEGIN
            UPDATE recording_retention_decisions SET expiry_at_ms=COALESCE(NEW.deadline_ms,-9223372036854775808),
                expiry_eligible=EXISTS(SELECT 1 FROM recording_files WHERE id=NEW.recording_id AND protected=0
                    AND finalized=1 AND cleanup_pending=0 AND retention_pending=0)
                WHERE recording_id=NEW.recording_id;
        END;")).await?;
    }
    connection.execute_batch("CREATE TRIGGER IF NOT EXISTS recording_retention_runtime_eligible
        AFTER UPDATE OF protected,finalized,cleanup_pending,retention_pending ON recording_files BEGIN
            UPDATE recording_retention_decisions SET expiry_eligible=(NEW.protected=0 AND NEW.finalized=1
                AND NEW.cleanup_pending=0 AND NEW.retention_pending=0) WHERE recording_id=NEW.id;
        END;").await?;
    Ok(())
}

pub(super) async fn sync_event_triggers(connection: &turso::Connection) -> Result<()> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_retention_policies WHERE uses_events=1 LIMIT 1",
            (),
        )
        .await?;
    let needed = rows.next().await?.is_some();
    drop(rows);
    if needed {
        event_triggers(connection).await?;
    } else {
        // ponytail: Remove unused event hooks instead of adding work to every event write.
        connection
            .execute_batch(
                "DROP TRIGGER IF EXISTS recording_retention_runtime_event_insert;
            DROP TRIGGER IF EXISTS recording_retention_runtime_event_update;
            DROP TRIGGER IF EXISTS recording_retention_runtime_event_delete;",
            )
            .await?;
    }
    Ok(())
}

async fn event_triggers(connection: &turso::Connection) -> Result<()> {
    for (name, event, versions) in [
        ("insert", "INSERT", &["NEW"][..]),
        (
            "update",
            "UPDATE OF kind,stream,start_time_ms,end_time_ms",
            &["OLD", "NEW"][..],
        ),
        ("delete", "DELETE", &["OLD"][..]),
    ] {
        let mut statements = String::new();
        for version in versions {
            statements.push_str(&event_enqueue_sql(version));
        }
        connection
            .execute_batch(format!(
                "CREATE TRIGGER IF NOT EXISTS recording_retention_runtime_event_{name}
            AFTER {event} ON recording_events BEGIN {statements} END;"
            ))
            .await?;
    }
    connection
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS recording_retention_work_turn
        ON recording_retention_camera_work(last_processed,camera_id);",
        )
        .await?;
    Ok(())
}

fn event_enqueue_sql(version: &str) -> String {
    let policy=format!("COALESCE((SELECT uses_events FROM recording_retention_policies WHERE camera_id={version}.camera_id),
        (SELECT uses_events FROM recording_retention_policies WHERE camera_id=''),0)=1");
    let upper=format!("CASE WHEN {version}.end_time_ms IS NULL THEN 9223372036854775807
        WHEN {version}.end_time_ms>{version}.start_time_ms THEN {version}.end_time_ms
        WHEN {version}.start_time_ms=9223372036854775807 THEN 9223372036854775807 ELSE {version}.start_time_ms+1 END");
    // ponytail: Coalesce at most 1,024 camera jobs; overflow fences a bounded global sweep.
    format!("UPDATE recording_retention_runtime SET overflow=1,overflow_next=1
        WHERE singleton=1 AND complete=0 AND request_pending=0 AND {policy}
            AND NOT EXISTS(SELECT 1 FROM recording_retention_camera_work WHERE camera_id={version}.camera_id)
            AND (SELECT count(*) FROM recording_retention_camera_work)>=1024;
        UPDATE recording_retention_runtime SET overflow=1,complete=0,cursor=NULL,
        high_water=(SELECT id FROM recording_files ORDER BY id DESC LIMIT 1)
        WHERE singleton=1 AND complete=1 AND request_pending=0 AND {policy} AND NOT EXISTS(SELECT 1 FROM recording_retention_camera_work WHERE camera_id={version}.camera_id)
            AND (SELECT count(*) FROM recording_retention_camera_work)>=1024;
        INSERT INTO recording_retention_camera_work(camera_id,lower_ms,upper_ms,generation)
        SELECT {version}.camera_id,{version}.start_time_ms,{upper},
            (SELECT generation+request_pending FROM recording_retention_runtime WHERE singleton=1) WHERE {policy}
            AND (EXISTS(SELECT 1 FROM recording_retention_camera_work WHERE camera_id={version}.camera_id)
                 OR (SELECT count(*) FROM recording_retention_camera_work)<1024)
        ON CONFLICT(camera_id) DO UPDATE SET
            next_lower_ms=CASE WHEN generation=excluded.generation THEN min(COALESCE(next_lower_ms,excluded.lower_ms),excluded.lower_ms) ELSE next_lower_ms END,
            next_upper_ms=CASE WHEN generation=excluded.generation THEN max(COALESCE(next_upper_ms,excluded.upper_ms),excluded.upper_ms) ELSE next_upper_ms END,
            queued_lower_ms=CASE WHEN generation!=excluded.generation THEN min(COALESCE(queued_lower_ms,excluded.lower_ms),excluded.lower_ms) ELSE queued_lower_ms END,
            queued_upper_ms=CASE WHEN generation!=excluded.generation THEN max(COALESCE(queued_upper_ms,excluded.upper_ms),excluded.upper_ms) ELSE queued_upper_ms END;")
}

async fn recording_triggers(connection: &turso::Connection) -> Result<()> {
    for (name, event) in [
        ("recording_retention_runtime_file_insert", "INSERT"),
        (
            "recording_retention_runtime_file_update",
            "UPDATE OF finalized,source_id,logical_stream_id,started_at_ms,ended_at_ms",
        ),
    ] {
        let generation = if event == "INSERT" {
            "(SELECT generation+request_pending FROM recording_retention_runtime WHERE singleton=1)"
        } else {
            "CASE WHEN OLD.retention_pending=1 THEN OLD.retention_generation ELSE
                (SELECT generation+request_pending FROM recording_retention_runtime WHERE singleton=1) END"
        };
        connection.execute_batch(format!("CREATE TRIGGER IF NOT EXISTS {name}
        AFTER {event} ON recording_files WHEN NEW.finalized=1 BEGIN
            INSERT INTO recording_retention_camera_spans(camera_id,span_ms)
                SELECT NEW.source_id,NEW.ended_at_ms-NEW.started_at_ms
                WHERE NEW.source_id IS NOT NULL AND NEW.ended_at_ms>NEW.started_at_ms
                    AND typeof(NEW.ended_at_ms-NEW.started_at_ms)='integer'
                ON CONFLICT(camera_id) DO UPDATE SET span_ms=max(span_ms,excluded.span_ms);
            UPDATE recording_files SET retention_pending=1,retention_generation={generation},retention_error=NULL
                WHERE id=NEW.id AND EXISTS(SELECT 1 FROM recording_retention_runtime WHERE settings_json IS NOT NULL);
        END;")).await?;
    }
    Ok(())
}

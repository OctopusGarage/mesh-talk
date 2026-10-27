use super::*;

/// Creation is restricted to an unpublished candidate (or a cfg(test) fixture).
pub(super) fn register(
    tx: &rusqlite::Transaction<'_>,
    domain: EventDomainId,
) -> Result<(), LogError> {
    require_schema(tx, true)?;
    tx.execute(
        "INSERT INTO event_domains(domain,next_commit) VALUES(?1,0) ON CONFLICT(domain) DO NOTHING",
        [domain.0.as_slice()],
    )?;
    Ok(())
}
/// Published authority is never repaired by creating an empty schema/domain.
pub(super) fn open_existing(c: &Connection, domain: EventDomainId) -> Result<(), LogError> {
    require_schema(c, false)?;
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM event_domains WHERE domain=?1)",
        [domain.0.as_slice()],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(LogError::IncompatibleEventSchema);
    }
    Ok(())
}
fn require_schema(tx: &Connection, initialize: bool) -> Result<(), LogError> {
    let marker: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_repository_schema')",
        [],
        |r| r.get(0),
    )?;
    if marker {
        // Verify the marker authority itself before trusting its typed values.
        require_owned_ddl(
            tx,
            "event_repository_schema",
            "table",
            SCHEMA
                .split(';')
                .next()
                .ok_or(LogError::IncompatibleEventSchema)?,
        )?;
        let mut rows = tx.prepare("SELECT singleton,version FROM event_repository_schema")?;
        let mut rows = rows.query([])?;
        let row = rows.next()?.ok_or(LogError::IncompatibleEventSchema)?;
        if row.get::<_, i64>(0)? != 1 || row.get::<_, i64>(1)? != 1 || rows.next()?.is_some() {
            return Err(LogError::IncompatibleEventSchema);
        }
    } else {
        if !initialize {
            return Err(LogError::IncompatibleEventSchema);
        }
        let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('event_domains','event_conversations','event_retirement_jobs','retained_events','retained_event_parents','event_heads','event_author_max','event_order_jobs','event_order_work','event_order_rows','event_order_current','event_profile_requests','event_profile_jobs','event_profile_candidates','event_profile_leaves'))", [], |r| r.get(0))?;
        if existing {
            return Err(LogError::IncompatibleEventSchema);
        }
        tx.execute_batch(SCHEMA)?;
    }
    validate_owned_ddl(tx)?;
    validate_objects(tx)?;
    // Prepare all shared selectors before admitting a new domain. Missing/changed
    // columns are storage errors, never successful absence.
    tx.prepare("SELECT domain,next_commit,event_count,estimated_bytes FROM event_domains LIMIT 0")?;
    tx.prepare("SELECT domain,conversation,destructive_epoch,revision,event_count,stored_bytes,max_lamport,v2_count,v3_count,v4_count,fingerprint_revision,fingerprint FROM event_conversations LIMIT 0")?;
    tx.prepare(&format!(
        "SELECT {HEADER_COLUMNS},body,commit_ordinal FROM retained_events LIMIT 0"
    ))?;
    tx.prepare("SELECT domain,child,parent FROM retained_event_parents LIMIT 0")?;
    tx.prepare("SELECT domain,conversation,id FROM event_heads LIMIT 0")?;
    tx.prepare("SELECT domain,conversation,author,max_seq FROM event_author_max LIMIT 0")?;
    Ok(())
}

// Large retained payloads use an integer-rowid table: WITHOUT ROWID seeks may
// materialize whole overflow records. Keep every scalar before the final BLOB;
// exact composite-PK and covering indexes contain only bounded metadata.
pub(super) const SCHEMA: &str = "
CREATE TABLE event_repository_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL);
INSERT INTO event_repository_schema VALUES(1,1);
CREATE TABLE event_domains(
 domain BLOB PRIMARY KEY CHECK(typeof(domain)='blob' AND length(domain)=16),
 next_commit INTEGER NOT NULL CHECK(typeof(next_commit)='integer' AND next_commit>=0),
 event_count INTEGER NOT NULL DEFAULT 0 CHECK(typeof(event_count)='integer' AND event_count>=0),
 source_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(source_revision)='integer' AND source_revision>=0),
 estimated_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(estimated_bytes)='integer' AND estimated_bytes>=0)
) WITHOUT ROWID;
CREATE TABLE event_conversations(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 lifecycle INTEGER NOT NULL DEFAULT 0 CHECK(typeof(lifecycle)='integer' AND lifecycle IN (0,1)),
 destructive_epoch INTEGER NOT NULL DEFAULT 0 CHECK(typeof(destructive_epoch)='integer' AND destructive_epoch>=0),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),
 event_count INTEGER NOT NULL CHECK(typeof(event_count)='integer' AND event_count>=0),
 stored_bytes INTEGER NOT NULL CHECK(typeof(stored_bytes)='integer' AND stored_bytes>=0),
 estimated_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(estimated_bytes)='integer' AND estimated_bytes>=0),
 max_lamport BLOB NOT NULL CHECK(typeof(max_lamport)='blob' AND length(max_lamport)=8),
 v2_count INTEGER NOT NULL CHECK(typeof(v2_count)='integer' AND v2_count>=0),
 v3_count INTEGER NOT NULL CHECK(typeof(v3_count)='integer' AND v3_count>=0),
 v4_count INTEGER NOT NULL CHECK(typeof(v4_count)='integer' AND v4_count>=0),
 fingerprint_revision INTEGER CHECK(fingerprint_revision IS NULL OR (typeof(fingerprint_revision)='integer' AND fingerprint_revision>=0)),
 fingerprint BLOB CHECK(fingerprint IS NULL OR (typeof(fingerprint)='blob' AND length(fingerprint)=32)),
 PRIMARY KEY(domain,conversation), FOREIGN KEY(domain) REFERENCES event_domains(domain)
) WITHOUT ROWID;
CREATE INDEX event_conversations_active ON event_conversations(domain,conversation) WHERE lifecycle=0 AND event_count>0;
CREATE TABLE event_retirement_jobs(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 store_uuid BLOB NOT NULL CHECK(typeof(store_uuid)='blob' AND length(store_uuid)=16),
 epoch INTEGER NOT NULL CHECK(typeof(epoch)='integer' AND epoch>=0),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),
 phase INTEGER NOT NULL DEFAULT 0 CHECK(typeof(phase)='integer' AND phase BETWEEN 0 AND 7),
 vertex BLOB CHECK(vertex IS NULL OR (typeof(vertex)='blob' AND length(vertex)=32)),
 vertex_commit INTEGER CHECK(vertex_commit IS NULL OR (typeof(vertex_commit)='integer' AND vertex_commit>=0)),
 auxiliary INTEGER NOT NULL DEFAULT 0 CHECK(typeof(auxiliary)='integer' AND auxiliary>=0),
 edges INTEGER NOT NULL DEFAULT 0 CHECK(typeof(edges)='integer' AND edges>=0),
 vertices INTEGER NOT NULL DEFAULT 0 CHECK(typeof(vertices)='integer' AND vertices>=0),
 work_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(work_bytes)='integer' AND work_bytes>=0),
 reclaimed_estimate INTEGER NOT NULL DEFAULT 0 CHECK(typeof(reclaimed_estimate)='integer' AND reclaimed_estimate>=0),
 deleted_body_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(deleted_body_bytes)='integer' AND deleted_body_bytes>=0),
 deleted_auxiliary INTEGER NOT NULL DEFAULT 0 CHECK(typeof(deleted_auxiliary)='integer' AND deleted_auxiliary>=0),
 deleted_vertices INTEGER NOT NULL DEFAULT 0 CHECK(typeof(deleted_vertices)='integer' AND deleted_vertices>=0),
 CHECK((vertex IS NULL AND vertex_commit IS NULL AND phase NOT IN (5,6)) OR (vertex IS NOT NULL AND vertex_commit IS NOT NULL AND phase IN (5,6))),
 PRIMARY KEY(domain,conversation), FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
) WITHOUT ROWID;
CREATE TABLE retained_events(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 author BLOB NOT NULL CHECK(typeof(author)='blob' AND length(author)=32),
 seq BLOB NOT NULL CHECK(typeof(seq)='blob' AND length(seq)=8),
 lamport BLOB NOT NULL CHECK(typeof(lamport)='blob' AND length(lamport)=8),
 wall_clock BLOB NOT NULL CHECK(typeof(wall_clock)='blob' AND length(wall_clock)=8),
 kind INTEGER NOT NULL CHECK(typeof(kind)='integer' AND kind BETWEEN 0 AND 18),
 parent_count INTEGER NOT NULL CHECK(typeof(parent_count)='integer' AND parent_count>=0),
 encoded_bytes INTEGER NOT NULL CHECK(typeof(encoded_bytes)='integer' AND encoded_bytes>=0),
 ciphertext_bytes INTEGER NOT NULL CHECK(typeof(ciphertext_bytes)='integer' AND ciphertext_bytes>=0),
 signature_bytes INTEGER NOT NULL CHECK(typeof(signature_bytes)='integer' AND signature_bytes>=0),
 stored_bytes INTEGER NOT NULL CHECK(typeof(stored_bytes)='integer' AND stored_bytes>=0),
 commit_ordinal INTEGER NOT NULL CHECK(typeof(commit_ordinal)='integer' AND commit_ordinal>=0),
 body BLOB NOT NULL CHECK(typeof(body)='blob' AND length(body)=stored_bytes),
 PRIMARY KEY(domain,id), UNIQUE(domain,conversation,author,seq), UNIQUE(domain,commit_ordinal),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
);
CREATE INDEX retained_events_clock ON retained_events(domain,conversation,lamport,id);
CREATE INDEX retained_events_commit ON retained_events(domain,conversation,commit_ordinal,id);
CREATE INDEX retained_events_ids ON retained_events(domain,conversation,id);
CREATE INDEX retained_events_wall_clock ON retained_events(domain,conversation,wall_clock,id);
CREATE INDEX retained_events_author_messages ON retained_events(domain,conversation,author,kind,lamport,id);
CREATE INDEX retained_events_kind_author ON retained_events(domain,conversation,kind,author,lamport,id);
CREATE TABLE retained_event_parents(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 child BLOB NOT NULL CHECK(typeof(child)='blob' AND length(child)=32),
 parent BLOB NOT NULL CHECK(typeof(parent)='blob' AND length(parent)=32),
 child_commit INTEGER NOT NULL CHECK(typeof(child_commit)='integer' AND child_commit>=0),
 PRIMARY KEY(domain,child,parent),
 FOREIGN KEY(domain,child) REFERENCES retained_events(domain,id),
 FOREIGN KEY(domain,parent) REFERENCES retained_events(domain,id)
) WITHOUT ROWID;
CREATE INDEX retained_event_children ON retained_event_parents(domain,parent,child);
CREATE INDEX retained_event_children_commit ON retained_event_parents(domain,parent,child_commit,child);
CREATE TABLE event_heads(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 PRIMARY KEY(domain,conversation,id),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation),
 FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id)
) WITHOUT ROWID;
CREATE INDEX event_heads_retained ON event_heads(domain,id);
CREATE TABLE event_author_max(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 author BLOB NOT NULL CHECK(typeof(author)='blob' AND length(author)=32),
 max_seq BLOB NOT NULL CHECK(typeof(max_seq)='blob' AND length(max_seq)=8),
 PRIMARY KEY(domain,conversation,author),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
) WITHOUT ROWID;
CREATE TABLE event_order_jobs(
 job BLOB PRIMARY KEY CHECK(typeof(job)='blob' AND length(job)=16),
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 order_kind INTEGER NOT NULL CHECK(typeof(order_kind)='integer' AND order_kind IN (0,1)),
 source_revision INTEGER NOT NULL CHECK(typeof(source_revision)='integer' AND source_revision>=0),
 target_count INTEGER NOT NULL CHECK(typeof(target_count)='integer' AND target_count>=0),
 base_count INTEGER NOT NULL CHECK(typeof(base_count)='integer' AND base_count>=0 AND base_count<=target_count),
 cutoff INTEGER NOT NULL CHECK(typeof(cutoff)='integer' AND cutoff>=0),
 phase INTEGER NOT NULL CHECK(typeof(phase)='integer' AND phase BETWEEN 0 AND 4),
 seed_after INTEGER NOT NULL CHECK(typeof(seed_after)='integer' AND seed_after>=-1),
 seeded INTEGER NOT NULL CHECK(typeof(seeded)='integer' AND seeded>=0 AND seeded<=base_count),
 next_rank INTEGER NOT NULL CHECK(typeof(next_rank)='integer' AND next_rank>=0 AND next_rank<=target_count),
 pop BLOB CHECK(pop IS NULL OR (typeof(pop)='blob' AND length(pop)=32)),
 child_after INTEGER NOT NULL CHECK(typeof(child_after)='integer' AND child_after>=-1),
 tail_after INTEGER NOT NULL CHECK(typeof(tail_after)='integer' AND tail_after>=cutoff),
 UNIQUE(domain,conversation,order_kind),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
) WITHOUT ROWID;
CREATE TABLE event_order_work(
 job BLOB NOT NULL CHECK(typeof(job)='blob' AND length(job)=16),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 priority BLOB NOT NULL CHECK(typeof(priority)='blob' AND length(priority)=8),
 remaining_degree INTEGER NOT NULL CHECK(typeof(remaining_degree)='integer' AND remaining_degree>=0),
 status INTEGER NOT NULL CHECK(typeof(status)='integer' AND status BETWEEN 0 AND 2),
 PRIMARY KEY(job,id), FOREIGN KEY(job) REFERENCES event_order_jobs(job)
) WITHOUT ROWID;
CREATE INDEX event_order_ready ON event_order_work(job,status,remaining_degree,priority,id);
CREATE TABLE event_order_rows(
 job BLOB NOT NULL CHECK(typeof(job)='blob' AND length(job)=16),
 rank INTEGER NOT NULL CHECK(typeof(rank)='integer' AND rank>=0),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 PRIMARY KEY(job,rank), UNIQUE(job,id), FOREIGN KEY(job) REFERENCES event_order_jobs(job)
) WITHOUT ROWID;
CREATE TABLE event_order_current(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 order_kind INTEGER NOT NULL CHECK(typeof(order_kind)='integer' AND order_kind IN (0,1)),
 source_revision INTEGER NOT NULL CHECK(typeof(source_revision)='integer' AND source_revision>=0),
 job BLOB NOT NULL CHECK(typeof(job)='blob' AND length(job)=16),
 PRIMARY KEY(domain,conversation,order_kind), FOREIGN KEY(job) REFERENCES event_order_jobs(job),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
) WITHOUT ROWID;
CREATE INDEX event_order_current_job ON event_order_current(job);
CREATE TABLE event_profile_requests(
 domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 PRIMARY KEY(domain,conversation),
 FOREIGN KEY(domain,conversation) REFERENCES event_conversations(domain,conversation)
) WITHOUT ROWID;
CREATE TABLE event_profile_jobs(
 job BLOB PRIMARY KEY CHECK(typeof(job)='blob' AND length(job)=16),
 domain BLOB NOT NULL UNIQUE CHECK(typeof(domain)='blob' AND length(domain)=16),
 source_revision INTEGER NOT NULL CHECK(typeof(source_revision)='integer' AND source_revision>=0),
 conversation BLOB CHECK(conversation IS NULL OR (typeof(conversation)='blob' AND length(conversation)=32)),
 after_author BLOB CHECK(after_author IS NULL OR (typeof(after_author)='blob' AND length(after_author)=32)),
 after_clock BLOB CHECK(after_clock IS NULL OR (typeof(after_clock)='blob' AND length(after_clock)=8)),
 after_id BLOB CHECK(after_id IS NULL OR (typeof(after_id)='blob' AND length(after_id)=32)),
 candidate_count INTEGER NOT NULL CHECK(typeof(candidate_count)='integer' AND candidate_count>=0),
 candidate_bytes INTEGER NOT NULL CHECK(typeof(candidate_bytes)='integer' AND candidate_bytes>=0),
 complete INTEGER NOT NULL CHECK(typeof(complete)='integer' AND complete IN (0,1)),
 CHECK((after_author IS NULL AND after_clock IS NULL AND after_id IS NULL) OR (conversation IS NOT NULL AND after_author IS NOT NULL AND after_clock IS NOT NULL AND after_id IS NOT NULL)),
 FOREIGN KEY(domain) REFERENCES event_domains(domain)
) WITHOUT ROWID;
CREATE TABLE event_profile_candidates(
 job BLOB NOT NULL CHECK(typeof(job)='blob' AND length(job)=16),
 conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 author BLOB NOT NULL CHECK(typeof(author)='blob' AND length(author)=32),
 estimated_bytes INTEGER NOT NULL CHECK(typeof(estimated_bytes)='integer' AND estimated_bytes>=0),
 PRIMARY KEY(job,conversation,id), FOREIGN KEY(job) REFERENCES event_profile_jobs(job)
) WITHOUT ROWID;
CREATE TABLE event_profile_leaves(
 job BLOB NOT NULL CHECK(typeof(job)='blob' AND length(job)=16),
 lamport BLOB NOT NULL CHECK(typeof(lamport)='blob' AND length(lamport)=8),
 id BLOB NOT NULL CHECK(typeof(id)='blob' AND length(id)=32),
 estimated_bytes INTEGER NOT NULL CHECK(typeof(estimated_bytes)='integer' AND estimated_bytes>=0),
 PRIMARY KEY(job,lamport,id), FOREIGN KEY(job) REFERENCES event_profile_jobs(job)
) WITHOUT ROWID;


";

// Constant-size schema inspection only: reopening must never recreate a missing
// authority table or run a history-sized CREATE INDEX as implicit repair.
fn validate_objects(c: &Connection) -> Result<(), LogError> {
    for (table, primary) in [
        ("event_repository_schema", &["singleton"][..]),
        ("event_domains", &["domain"][..]),
        ("event_conversations", &["domain", "conversation"][..]),
        ("event_retirement_jobs", &["domain", "conversation"][..]),
        ("retained_events", &["domain", "id"][..]),
        ("retained_event_parents", &["domain", "child", "parent"][..]),
        ("event_order_jobs", &["job"][..]),
        ("event_profile_requests", &["domain", "conversation"][..]),
        ("event_profile_jobs", &["job"][..]),
        (
            "event_profile_candidates",
            &["job", "conversation", "id"][..],
        ),
        ("event_profile_leaves", &["job", "lamport", "id"][..]),
        ("event_order_work", &["job", "id"][..]),
        ("event_order_rows", &["job", "rank"][..]),
        (
            "event_order_current",
            &["domain", "conversation", "order_kind"][..],
        ),
        ("event_heads", &["domain", "conversation", "id"][..]),
        (
            "event_author_max",
            &["domain", "conversation", "author"][..],
        ),
    ] {
        let kind: Option<String> = c
            .query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [table],
                |r| r.get(0),
            )
            .optional()?;
        if kind.as_deref() != Some("table") {
            return Err(LogError::IncompatibleEventSchema);
        }
        let mut stmt =
            c.prepare("SELECT name FROM pragma_table_info(?1) WHERE pk>0 ORDER BY pk")?;
        let mut rows = stmt.query([table])?;
        for expected in primary {
            let Some(row) = rows.next()? else {
                return Err(LogError::IncompatibleEventSchema);
            };
            if row.get::<_, String>(0)? != *expected {
                return Err(LogError::IncompatibleEventSchema);
            }
        }
        if rows.next()?.is_some() {
            return Err(LogError::IncompatibleEventSchema);
        }
    }
    for (name, table, columns) in [
        (
            "event_conversations_active",
            "event_conversations",
            &["domain", "conversation"][..],
        ),
        ("event_heads_retained", "event_heads", &["domain", "id"][..]),
        (
            "event_order_current_job",
            "event_order_current",
            &["job"][..],
        ),
        (
            "retained_events_commit",
            "retained_events",
            &["domain", "conversation", "commit_ordinal", "id"][..],
        ),
        (
            "retained_event_children_commit",
            "retained_event_parents",
            &["domain", "parent", "child_commit", "child"][..],
        ),
        (
            "event_order_ready",
            "event_order_work",
            &["job", "status", "remaining_degree", "priority", "id"][..],
        ),
        (
            "retained_events_clock",
            "retained_events",
            &["domain", "conversation", "lamport", "id"][..],
        ),
        (
            "retained_events_ids",
            "retained_events",
            &["domain", "conversation", "id"][..],
        ),
        (
            "retained_events_wall_clock",
            "retained_events",
            &["domain", "conversation", "wall_clock", "id"][..],
        ),
        (
            "retained_events_author_messages",
            "retained_events",
            &["domain", "conversation", "author", "kind", "lamport", "id"][..],
        ),
        (
            "retained_events_kind_author",
            "retained_events",
            &["domain", "conversation", "kind", "author", "lamport", "id"][..],
        ),
        (
            "retained_event_children",
            "retained_event_parents",
            &["domain", "parent", "child"][..],
        ),
    ] {
        let actual: Option<(String, String)> = c
            .query_row(
                "SELECT type,tbl_name FROM sqlite_master WHERE name=?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if actual
            .as_ref()
            .map(|(kind, owner)| (kind.as_str(), owner.as_str()))
            != Some(("index", table))
        {
            return Err(LogError::IncompatibleEventSchema);
        }
        let partial: i64 = c.query_row(
            "SELECT partial FROM pragma_index_list(?1) WHERE name=?2",
            params![table, name],
            |r| r.get(0),
        )?;
        if partial != i64::from(name == "event_conversations_active") {
            return Err(LogError::IncompatibleEventSchema);
        }
        let mut stmt = c.prepare(
            "SELECT name,desc,coll FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno",
        )?;
        let mut rows = stmt.query([name])?;
        for expected in columns {
            let Some(row) = rows.next()? else {
                return Err(LogError::IncompatibleEventSchema);
            };
            if row.get::<_, String>(0)? != *expected
                || row.get::<_, i64>(1)? != 0
                || row.get::<_, String>(2)? != "BINARY"
            {
                return Err(LogError::IncompatibleEventSchema);
            }
        }
        if rows.next()?.is_some() {
            return Err(LogError::IncompatibleEventSchema);
        }
    }
    Ok(())
}

// This compares our fixed versioned DDL, not arbitrary SQL grammar. SQLite keeps
// each submitted CREATE statement in sqlite_master. Collapse outside-literal
// whitespace runs while preserving token boundaries, and trim the statement
// delimiter; never concatenate SQL tokens by removing their separator. A known
// version with a changed type, CHECK, FK, UNIQUE or collation is not adoptable.
fn normalized_owned_ddl(sql: &str) -> String {
    let mut normalized = String::new();
    let mut quoted = None;
    let mut chars = sql.trim().trim_end_matches(';').trim().chars().peekable();
    while let Some(character) = chars.next() {
        if let Some(quote) = quoted {
            normalized.push(character);
            if character == quote {
                if chars.peek() == Some(&quote) {
                    normalized.push(chars.next().expect("peeked quote exists"));
                } else {
                    quoted = None;
                }
            }
        } else if character == '\'' || character == '"' {
            quoted = Some(character);
            normalized.push(character);
        } else if character.is_ascii_whitespace() {
            if !normalized.ends_with(' ') {
                normalized.push(' ');
            }
        } else {
            normalized.push(character);
        }
    }
    normalized
}
fn require_owned_ddl(
    c: &Connection,
    name: &str,
    kind: &str,
    expected: &str,
) -> Result<(), LogError> {
    let actual: Option<String> = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1 AND type=?2",
            params![name, kind],
            |row| row.get(0),
        )
        .optional()?;
    if actual.as_deref().map(normalized_owned_ddl).as_deref()
        != Some(normalized_owned_ddl(expected).as_str())
    {
        return Err(LogError::IncompatibleEventSchema);
    }
    Ok(())
}
fn validate_owned_ddl(c: &Connection) -> Result<(), LogError> {
    for statement in SCHEMA.split(';').map(str::trim) {
        if let Some(table) = statement.strip_prefix("CREATE TABLE ") {
            let name = table
                .split_once('(')
                .ok_or(LogError::IncompatibleEventSchema)?
                .0
                .trim();
            require_owned_ddl(c, name, "table", statement)?;
        } else if let Some(index) = statement.strip_prefix("CREATE INDEX ") {
            let name = index
                .split_once(" ON ")
                .ok_or(LogError::IncompatibleEventSchema)?
                .0
                .trim();
            require_owned_ddl(c, name, "index", statement)?;
        }
    }
    Ok(())
}

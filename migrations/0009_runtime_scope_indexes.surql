DEFINE INDEX source_scope ON memory_source FIELDS owner_id, project_id, recorded_at_ms;
DEFINE INDEX consumption_scope ON memory_source_consumption FIELDS owner_id, project_id;
DEFINE INDEX job_scope ON memory_job FIELDS owner_id, project_id;
DEFINE INDEX revision_lineage ON memory_revision FIELDS owner_id, project_id, memory_id;
DEFINE INDEX lifecycle_audit_scope ON memory_lifecycle_audit FIELDS owner_id, project_id, revision_id;

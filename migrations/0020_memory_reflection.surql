DEFINE FIELD IF NOT EXISTS basis ON memory_tree_revision TYPE string;
DEFINE FIELD IF NOT EXISTS confidence ON memory_tree_revision TYPE option<string> READONLY;
DEFINE FIELD IF NOT EXISTS derived_from ON memory_tree_revision TYPE array<string>;
UPDATE memory_tree_revision SET basis = 'observed', derived_from = [] WHERE basis = NONE;
DEFINE FIELD OVERWRITE basis ON memory_tree_revision TYPE string DEFAULT 'observed' READONLY;
DEFINE FIELD OVERWRITE derived_from ON memory_tree_revision TYPE array<string> DEFAULT [] READONLY;

DEFINE TABLE IF NOT EXISTS memory_reflection_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS owner_id ON memory_reflection_state TYPE string;
DEFINE FIELD IF NOT EXISTS project_id ON memory_reflection_state TYPE string;
DEFINE FIELD IF NOT EXISTS branch_memory ON memory_reflection_state TYPE string;
DEFINE FIELD IF NOT EXISTS observed_revisions ON memory_reflection_state TYPE int;
DEFINE FIELD IF NOT EXISTS baseline ON memory_reflection_state TYPE int;
DEFINE FIELD IF NOT EXISTS last_run_ms ON memory_reflection_state TYPE int;
DEFINE FIELD IF NOT EXISTS mature_lessons_seen ON memory_reflection_state TYPE array<string>;
DEFINE INDEX IF NOT EXISTS reflection_branch ON memory_reflection_state FIELDS owner_id, project_id, branch_memory UNIQUE;

DEFINE TABLE IF NOT EXISTS memory_contradiction SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS owner_id ON memory_contradiction TYPE string;
DEFINE FIELD IF NOT EXISTS project_id ON memory_contradiction TYPE string;
DEFINE FIELD IF NOT EXISTS concluded_memory ON memory_contradiction TYPE string;
DEFINE FIELD IF NOT EXISTS observed_memory ON memory_contradiction TYPE string;
DEFINE FIELD IF NOT EXISTS created_at_ms ON memory_contradiction TYPE int;
DEFINE INDEX IF NOT EXISTS contradiction_pair ON memory_contradiction FIELDS owner_id, project_id, concluded_memory, observed_memory UNIQUE;

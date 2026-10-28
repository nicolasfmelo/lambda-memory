DEFINE TABLE memory_tree_migrated SCHEMAFULL;
DEFINE FIELD owner_id ON memory_tree_migrated TYPE string READONLY;
DEFINE FIELD project_id ON memory_tree_migrated TYPE string READONLY;
DEFINE FIELD source_id ON memory_tree_migrated TYPE string READONLY;
DEFINE FIELD operation_id ON memory_tree_migrated TYPE string READONLY;
DEFINE INDEX tree_migrated_source ON memory_tree_migrated FIELDS owner_id, project_id, source_id UNIQUE;

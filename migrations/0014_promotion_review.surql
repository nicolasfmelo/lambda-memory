DEFINE TABLE memory_promotion_review SCHEMAFULL;
DEFINE FIELD owner_id ON memory_promotion_review TYPE string READONLY;
DEFINE FIELD project_id ON memory_promotion_review TYPE string READONLY;
DEFINE FIELD memory_id ON memory_promotion_review TYPE string READONLY;
DEFINE FIELD revision_id ON memory_promotion_review TYPE string READONLY;
DEFINE FIELD verdict ON memory_promotion_review TYPE string ASSERT $value IN ['user', 'project'] READONLY;
DEFINE FIELD decided_at_ms ON memory_promotion_review TYPE int READONLY;
DEFINE FIELD retired_at_ms ON memory_promotion_review TYPE option<int>;
DEFINE INDEX promotion_review_identity ON memory_promotion_review FIELDS owner_id, project_id, memory_id, revision_id UNIQUE;

DEFINE FIELD recovery_of ON memory_runtime_operation TYPE option<array<string>> READONLY;
DEFINE FIELD authorized_by_actor ON memory_runtime_operation TYPE option<string> READONLY;
DEFINE FIELD authorized_at_ms ON memory_runtime_operation TYPE option<int> READONLY;

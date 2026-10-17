"""Small conservative static audit; compiler/tests remain the semantic check."""
from pathlib import Path
import re
root = Path(__file__).resolve().parents[1] / "src"
allowed = {
    "domain_models": {"domain_models"},
    "engines": {"engines", "domain_models"},
    "integrations": {"integrations", "domain_models"},
    "services": {"services", "engines", "integrations", "domain_models"},
    "delivery": {"delivery", "services", "integrations", "domain_models"},
}
failures = []
for layer, permitted in allowed.items():
    for path in (root / layer).rglob("*.rs"):
        text = path.read_text()
        for target in re.findall(r"crate::([a-z_]+)::", text):
            if target not in permitted:
                failures.append(f"{path.relative_to(root)} imports forbidden layer {target}")
        if layer in {"domain_models", "engines"} and re.search(r"std::(?:fs|net|process)|Instant::|SystemTime::|print!|println!", text):
            failures.append(f"{path.relative_to(root)} contains an I/O/time side effect")
        if layer in {"engines", "services"} and re.search(r"WireRequest|WireAnswer|serde_json|SurrealQuery|SELECT .* FROM", text):
            failures.append(f"{path.relative_to(root)} leaks infrastructure data")
        if layer == "delivery" and re.search(r"\.search\(|\.evaluate\(|\.plan\(|recall_policy", text):
            failures.append(f"{path.relative_to(root)} bypasses service or contains policy")
if failures:
    raise SystemExit("\n".join(failures))
print("MASA layer audit passed: explicit crate imports, pure layers, private wire DTOs, thin delivery")

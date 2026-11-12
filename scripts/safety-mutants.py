#!/usr/bin/env python3
"""Prove the safety tests fail loudly if any guard is removed (#35).

Each mutation below disables one safety check in `crates/safety/src`. For
every one, this script applies it, runs `cargo test -p storagelens-safety`,
and expects the tests to FAIL. A mutation the tests survive means a guard
could be deleted without anyone noticing, so the script exits non-zero.

The original file is restored after every mutation, even on Ctrl-C.

Run from the repository root:

    python3 scripts/safety-mutants.py            # all mutations
    python3 scripts/safety-mutants.py bundle     # names containing "bundle"
"""

import subprocess
import sys
from pathlib import Path

SRC = Path("crates/safety/src")

# (name, file, original code, mutated code). Each original must occur once.
MUTATIONS = [
    ("guard: traversal before resolving", "guard.rs",
     "            return Err(Refusal::Traversal);\n        }\n        let metadata",
     "            // mutated\n        }\n        let metadata"),
    ("guard: traversal in resolved paths", "guard.rs",
     "    if components(platform, path).any(|part| part == \".\" || part == \"..\") {\n        return Err(Refusal::Traversal);",
     "    if components(platform, path).any(|part| part == \".\" || part == \"..\") {\n        // mutated"),
    ("guard: outside the root", "guard.rs",
     "        if !inside {\n            return Err(Refusal::OutsideRoot);\n        }",
     "        if !inside {\n            // mutated\n        }"),
    ("guard: the root itself", "guard.rs",
     "        if is_root {\n            return Err(Refusal::IsRoot);\n        }",
     "        if is_root {\n            // mutated\n        }"),
    ("guard: system locations", "guard.rs",
     "return Err(Refusal::System(location.shown));",
     "let _ = location.shown;"),
    ("guard: protected home folders", "guard.rs",
     "Relation::Equal | Relation::Ancestor => return Err(Refusal::Protected),",
     "Relation::Equal | Relation::Ancestor => {}"),
    ("guard: never-touch names", "guard.rs",
     "return Err(Refusal::NeverTouch(name));",
     "let _ = name;"),
    ("guard: bundle insides", "guard.rs",
     "return Err(Refusal::BundleInternal);",
     "{}"),
    ("guard: symlink target", "guard.rs",
     "Ok(target) => self.check_path(&target),",
     "Ok(_target) => Ok(()),"),
    ("guard: symlink itself", "guard.rs",
     "self.check_path(&link)?;",
     "let _ = &link;"),
    ("preflight: kind", "preflight.rs",
     "if found.kind != expected.kind {",
     "if false && found.kind != expected.kind {"),
    ("preflight: replaced (volume, file id)", "preflight.rs",
     "if found.dev != expected.dev || found.file_id != expected.file_id {",
     "if false {"),
    ("preflight: size", "preflight.rs",
     "if found.kind == EntryKind::File && found.size_logical != expected.size_logical {",
     "if false {"),
    ("preflight: modified time", "preflight.rs",
     "if found.modified != expected.modified {",
     "if false {"),
    ("preflight: locked flags", "preflight.rs",
     "if found.attrs & locked != 0 {",
     "if false && found.attrs & locked != 0 {"),
    ("trash: manifest before moving", "trash.rs",
     "    let path = new_path(manifest_dir, now);\n    manifest.write(&path)?;",
     "    let path = new_path(manifest_dir, now);"),
    ("trash: guard before each move", "trash.rs",
     "} else if let Err(refusal) = guard.check(&item.path) {",
     "} else if let Err(refusal) = Ok::<(), Refusal>(()) {"),
    ("trash: preflight before each move", "trash.rs",
     "} else if let Err(skip) = preflight(&item.path, &item.stamp, stamp) {",
     "} else if let Err(skip) = Ok::<(), Skip>(()) {"),
    ("trash: stop between items", "trash.rs",
     "cancelled = cancelled || !before(index);",
     "let _ = before(index);"),
    ("undo: never over something new", "undo.rs",
     "Some(_) if fs::symlink_metadata(&item.path).is_ok() => Undone::Occupied,",
     "Some(_) if false => Undone::Occupied,"),
    ("bin: restore never overwrites", "bin.rs",
     "    if fs::symlink_metadata(original).is_ok() {",
     "    if false {"),
]


def run_tests() -> str:
    """'failed', 'passed', or 'did not compile'."""
    result = subprocess.run(
        ["cargo", "test", "-q", "-p", "storagelens-safety"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    output = result.stdout + result.stderr
    if result.returncode == 0:
        return "passed"
    if "error[E" in output or "could not compile" in output and "test result" not in output:
        return "did not compile"
    return "failed"


def main() -> int:
    wanted = sys.argv[1:]
    chosen = [m for m in MUTATIONS if not wanted or any(w in m[0] for w in wanted)]
    if run_tests() != "passed":
        print("The safety tests fail without any mutation; fix them first.")
        return 2

    survivors = []
    for name, file, original, mutated in chosen:
        path = SRC / file
        text = path.read_text(encoding="utf-8")
        if text.count(original) != 1:
            print(f"?  {name}: the code to mutate was not found exactly once in {file}")
            survivors.append(name)
            continue
        try:
            path.write_text(text.replace(original, mutated), encoding="utf-8")
            outcome = run_tests()
        finally:
            path.write_text(text, encoding="utf-8")
        mark = "ok" if outcome == "failed" else "!!"
        print(f"{mark} {name}: tests {outcome}")
        if outcome != "failed":
            survivors.append(name)

    print()
    if survivors:
        print(f"{len(survivors)} of {len(chosen)} mutations were NOT caught:")
        for name in survivors:
            print(f"   - {name}")
        return 1
    print(f"All {len(chosen)} mutations were caught: removing any guard fails the tests.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

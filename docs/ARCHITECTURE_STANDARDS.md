# Architecture and review-size standards

ReMagic is a small system layer, not a single application. Source boundaries
therefore follow ownership and lifecycle responsibilities rather than UI
screens or arbitrary line-count slices.

## Mandatory boundaries

- `remagicd` owns policy, state transitions, process supervision, and recovery.
- `remagic-display-host` is the only owner of panel and raw input devices.
- `remagic-runner` prepares one application's runtime and bridges lifecycle.
- applications communicate through versioned protocols and never reach around
  the manager to acquire system-owned devices.
- protocol models, platform I/O, state machines, business rules, and tests live
  in separate modules.

## Review-size budget

- Production files target at most 400 physical lines.
- 401–500 lines produce a warning and should be split at the next coherent
  responsibility boundary.
- Production files over 500 lines fail by default. A cohesive file that is
  genuinely clearer intact may receive one exact-path exception in
  `architecture-exceptions.tsv`, with a reviewed upper bound and concrete
  rationale. Globs and directory-wide exemptions are forbidden.
- Test/fixture files may contain up to 800 lines.
- Generated code, vendored sources, and upstream patch payloads are excluded and
  must remain isolated in clearly named directories.
- Functions target at most 60 lines; production Rust functions over 100 lines
  fail. A long operation must be expressed as named, testable phases.

Run `scripts/check-architecture.sh` before committing. A numerical pass is
necessary but not sufficient: reviewers must still reject files that combine
unrelated responsibilities, and should remove an exception when its rationale
no longer applies.

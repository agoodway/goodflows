## MODIFIED Requirements

### Requirement: cr validate checks claims as cr open and cr claims read them
Refs: D8, G2
A claims file SHALL get every problem `cr open` and `cr claims` would refuse, plus `<file>: the front matter needs a top-level cr: null line for cr open to fill in` when `cr` is absent. It SHALL warn `<file>: head is <h> but HEAD is <sha>; cr claims uploads claims only for HEAD` and, when `cr` is null without a title, `<file>: the body has no # heading, so cr open needs --title`. It SHALL also warn, naming the key or entry, about each rule of the claims format the server refuses that the file breaks among: a missing or unknown front matter key, a `door` other than `one_way` or `two_way`, a `proof` other than `stated`, `pointed`, `walked` or `ran`, a `ran` fact with no `evidence`, and a question whose `recommendation` is not one of its `options`. Warnings SHALL NOT fail the file.

#### Scenario: Other head
- **WHEN** `cr.md` has `cr: 4` and `head: abc`
- **THEN** its entry has `ok` true and the head warning, and `cr validate` on it alone exits 0

#### Scenario: No cr line
- **WHEN** `other.md` has `format: change-claims/2` and the current `head` but no `cr`
- **THEN** its error is `<file>: the front matter needs a top-level cr: null line for cr open to fill in`

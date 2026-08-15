# Architecture

The workspace is four crates in a line. Dependencies point one way only:

```
dbui  ──▶  dbui-ui  ──▶  dbui-app  ──▶  dbui-driver  ──▶  dbui-domain
(bin)      (gpui)        (use cases)    (sqlx)            (no deps)
```

`cargo build` is what enforces it. A crate cannot reach past its neighbour
because the manifest does not list it, so the boundaries are not a convention
anyone has to remember.

## The layers

### `dbui-domain` — the model

Connections, catalog, values, result sets. No I/O, no async, no database
client, no UI. Its only dependency is `serde`, and that is only so the same
structs can be persisted verbatim.

This is where both adapters and the UI agree on what a column, a cell and a
table *are*. `Value` is the important one: every engine-native type is widened
into it, so a `BIGINT` from MySQL and an `int8` from Postgres reach the grid as
the same thing.

**Test rule:** everything here is a pure function, so everything here is unit
tested.

### `dbui-driver` — the port and its adapters

One trait, `DatabaseDriver`, and three implementations of it. Everything
engine-specific lives here and nowhere else:

- connection options and TLS modes
- introspection SQL (`pg_catalog` vs `information_schema` vs `sqlite_master`)
- decoding wire values into `Value`
- turning a `sqlx::Error` into a sentence worth showing someone

`dbui_driver::connect` is the only function in the codebase that names a
concrete adapter. Everyone else holds an `Arc<dyn DatabaseDriver>`.

**SQLite was added exactly as this predicted**: a `Driver` variant, an adapter
module, one arm in `connect`. The UI changed in two places, and neither was
about SQL — the connection form hides host, port, user and password for a
file-based engine, and validation asks for a path instead. Everything else
above the port was untouched.

Its tests are the ones worth copying: they need no server, because the engine
is linked in and the database is a temp file the test makes and deletes. So
`crates/dbui-driver/tests/sqlite.rs` runs on every `cargo test` and proves the
same things `live.rs` can only prove when someone has Docker running.

**Every engine-specific behaviour is proved on every engine.** `live.rs` runs
each of its tests twice, once per server; `sqlite.rs` mirrors them against a
file. The same fifteen properties -- catalog, column metadata, value decoding,
stable paging, server-side sorting, batch commit and rollback, foreign keys,
generated DDL, read-only enforcement, identifier quoting -- are asserted three
times over. An adapter that passes its own tests and nobody else's is how two
engines drift into behaving differently under one UI.

### `dbui-app` — use cases and state

The whole application minus its pixels. It owns:

- `DbRuntime` — the tokio runtime, and the only way to reach the database
- `commands` — one function per use case, each returning a `Task` to await
- `Workspace` — what connections exist, which are open as tabs, and what is
  known about each
- `store` — saved connections on disk
- `session` — which connection tabs were open, and what each had open in them

No `gpui`. That is the constraint that keeps the use cases free of rendering
concerns, and it means all of this is reachable from a plain `#[test]`.

### `dbui-ui` — the front-end

Opens a window, draws the state `dbui-app` holds, turns clicks and keys back
into use cases. All mutable window state lives in `root.rs` on `DbUi`; the
modules under `components/` are `impl DbUi` blocks that only render.

That split is deliberate. Widgets that own state are how a UI ends up with two
answers to the same question.

## Threading

GPUI renders on the main thread and its executor is not `Send`. sqlx needs a
tokio reactor. Rather than make one drive the other:

```
main thread                       tokio worker threads
───────────                       ────────────────────
cx.spawn(async { ... })
  task.await  ◀── oneshot ──────  driver.execute(sql).await
  this.update(cx, ...)
```

`DbRuntime::spawn` puts the work on tokio and hands back a `Task<T>` — a future
over a oneshot channel — which the UI awaits inside `cx.spawn`. A query in
flight never blocks a frame, and a task that is dropped (window closed) simply
resolves to `None`.

Every database call goes through `DbRuntime::spawn`. Nothing else may block the
main thread on I/O.

## Decisions worth knowing

**Values are widened, not passed through.** The grid never sees a driver-native
type. A type this build has no decoder for becomes `Value::Unsupported` carrying
its type name — a result set with one odd column still shows the other forty.

**Exact numerics stay strings.** `NUMERIC` and `DECIMAL` become
`Value::Decimal(String)`, never `f64`. Those columns are usually money, and
rounding them on the way to a screen is a display bug that looks like a data
bug. SQLite is the exception, and not by choice: it has no exact numeric type
to preserve — see Known limits.

**Every table read is ordered.** `LIMIT`/`OFFSET` over an unordered read is not
pagination: neither engine promises a row order without an `ORDER BY`, so the
same row can arrive on two pages while another never arrives at all. The app
layer therefore reads the columns first and orders by the primary key, putting
the user's chosen sort in front of it rather than replacing it — a sort on a
column full of duplicates is not a total order either, and a page boundary
inside a run of equal values is exactly where rows go missing.

**Identifiers are quoted, never bound.** SQL parameters cannot be identifiers,
so generated statements have to paste table names in. Every one goes through
`TableRef::quoted`, which escapes by doubling the quote character. There is a
test that a hostile table name cannot break out.

**Paging probes with `limit + 1`.** Fetching one row more than asked for and
discarding it answers "is there more?" without a second `COUNT(*)` round trip.

**Passwords are not written to disk.** `ConnectionConfig::password` is
`skip_serializing`. Wiring in the OS keychain later means changing `store.rs`
and nothing else.

**One draft speaks for the whole selection.** `RowDraft` holds a list of rows
rather than one, and editing several is the same object with more indices in
it. A column the rows agree on shows that value; one they disagree on shows
`MIXED`, which is a write token like `NULL` and `DEFAULT` — the box always says
exactly what will be written, and a field still reading `MIXED` is written to
nobody. That is what keeps "edit a row" and "edit a selection" from becoming
two staging paths, the second of which would be the one nobody tested.

Staging is recomputed rather than merged: `to_pending_batch` returns the whole
of what each row should end up with, including the columns the draft
deliberately left alone, and the caller clears those rows out before extending
with it. Merging instead would double-count — and, worse, a `MIXED` column over
rows staged with *different* values would look like an instruction to drop what
was already there.

**Connection tabs are two lists, not one.** `Workspace` keeps `entries` (every
saved connection) apart from `open` (the ones with a tab, in tab order). Tab
order is the user's arrangement and has nothing to do with the order
connections were created in, and closing a tab must not delete a server. Both
invariants — every open id names an entry, `active` is always one of them —
are maintained inside `Workspace` rather than trusted to callers.

**Each connection tab owns its table tabs.** The front connection's `Tabs` live
on `DbUi::tabs` and every other one's in `DbUi::stashed_tabs`; switching is a
swap between the two. Keeping the active set in the same field it always
occupied is what let the ~60 places that say `self.tabs` stay as they were —
a `HashMap` lookup at each would have been the same behaviour spelled worse.

**The session is a cache, and is treated like one.** `session.json` is separate
from `connections.json` because losing the former is cosmetic and losing the
latter is not; a session that will not parse opens an empty tab bar rather than
taking the launch down with it. Every id in it is checked against the saved
connections on load, so a deleted connection cannot leave a tab pointing at a
server that is gone. It stores the *question* each tab was asking — table,
filter, hidden columns, SQL text — and never the rows, which would show
yesterday's data under a live heading.

**The session is written by rename, not in place.** It is saved on every tab
click, and a plain write truncates before it fills: a crash — or a concurrent
read — during that window sees half a file. Writing a temp file and renaming it
over the old one is atomic on the same filesystem, so a reader gets one whole
session or the other.

**Restoring reconnects one connection, not all of them.** Only the tab that was
in front dials out; the rest connect when clicked. Coming back from lunch is
not a reason to reach for every server the user has ever saved, including the
production one they left open last week.

**`DBUI_CONFIG_DIR` overrides the configuration directory.** It is what lets a
second profile exist side by side, and what keeps the UI tests — which persist
a session as they click around — out of the developer's own configuration.

## Testing

Three layers of test, matching the three kinds of thing that can be wrong.

**Unit tests** live beside the code they cover, in the crate they belong to.
Everything in `dbui-domain` is a pure function, so everything in it is tested:
identifier quoting, statement classification, paging arithmetic, value
rendering. Same for `Workspace` and `store`.

**End-to-end UI tests** (`crates/dbui-ui/src/e2e.rs`) open a real GPUI window
through `TestAppContext` and dispatch real keystrokes. Every command on `DbUi`
is reached by one, either by name or through the key that triggers it — the
list was arrived at by auditing the two against each other rather than by
assuming, and the audit found a shortcut nobody had ever driven.

One of them takes `layout_lock()`: the zoom lives in a process-wide static
because it scales every surface at once, so the tests that measure painted
pixels cannot run alongside the one that moves it.

**Every surface is also drawn, not just driven.** The `*_draws` tests put the
window into each state — the expanded change bubble with all three kinds of
staged change in it, each palette, the context menu against a corner, the
inline cell editor, every theme — and repaint at three window sizes including
a narrow and a short one. A layout that divides by a zero width or an element
id that collides only surfaces when something actually paints it, and until
these existed the only way to find one was to open the app and look.

They were checked the way any test should be: by breaking each render path in
turn and confirming the test that covers it fails. That found two that were
drawing nothing — the change bubble's diff (staging a row already expands the
bubble, so the test's `toggle` closed it again) and the schema tree, which
renders only when a connection is *live* and so had never been painted by any
test at all.

**A key binding is dispatched before `on_key` ever sees it.** `key_bindings()`
is shared by `run()` and the test harness for that reason: a shortcut handled
in both places behaves differently in a window that bound them and a test that
did not, which is exactly how ⌘↵ came to follow a foreign key in the suite and
run a query in the app. Installing the real bindings in the tests is what makes
the two agree.

**The tree is why `open_connected` exists.** SQLite makes a real connection
available in a unit test — the engine is linked in and the database is a temp
file — so the surfaces gated on connection status can be reached without a
server and without touching anything outside the test process. They are in-crate rather
than in `tests/` so they can read `pub(crate)` state without widening the public
API for the benefit of tests.

They exist because of a bug no unit test could have caught: focus set at
construction is lost if the window is not key yet, `on_key_down` then never
fires, and the app draws perfectly with every shortcut dead.
`shortcuts_survive_the_window_losing_focus` blurs the window and types, which
fails without the re-focus in `Render::render` and passes with it. The rest of
the suite passes either way — the test platform's window *is* key at
construction — so that one test is the only thing standing between this and a
silent regression.

One wrinkle worth knowing: `Keystroke::parse("a")` sets `key` but leaves
`key_char` empty, and typed text is read from `key_char` (the only field that is
right for shifted keys and non-US layouts). Tests spell typing as `a->a`, which
is the harness's way of saying the platform delivered that character. The
`typing()` helper does it.

**Live tests** (`crates/dbui-driver/tests/live.rs`) talk to real servers. They
are the only tests that can prove the introspection SQL parses, the decoders
match what the wire sends, and the generated statements are accepted — none of
which can be faked convincingly, so nothing there is mocked.

They are opt-in behind `DBUI_LIVE_TESTS=1` so a checkout with no servers stays
green, but with the flag set an unreachable server *fails*: silently skipping
when someone asked for them defeats the point. Each test builds fixtures in a
schema named after itself, because cargo runs them in parallel and a shared
fixture breaks twice over — two tests racing to create it, and one test's
`DELETE` changing another's row count.

### What the live tests caught

Every one of these is invisible to a unit test, and two of them are the kind of
bug a user meets in the first five minutes:

- **MySQL 8 could not connect at all.** It defaults to `caching_sha2_password`,
  which needs TLS or an RSA key exchange to send the password. sqlx gates that
  behind `mysql-rsa`; without it every TLS-disabled connection fails — which is
  what every local development server looks like.
- **`numeric(10,2)` rendered as `0.1000`.** Postgres stores numerics in
  base-10000 groups and sqlx's `BigDecimal` conversion reports a scale rounded
  up to a multiple of four digits. `rust_decimal` honours the wire's display
  scale, so money columns now show the scale they were declared with;
  `BigDecimal` stays as the fallback for numerics too large for it.
- **`text[]` did not decode.** sqlx names array types by element with a `[]`
  suffix (`TEXT[]`), not the `_text` spelling `pg_type` uses internally. The
  decoder matched the wrong one and every array fell through to
  `Value::Unsupported` — which is at least how the failure stayed legible.
- **Every row edit failed on PostgreSQL.** Bound values all went over as
  strings, so `UPDATE … WHERE "id" = $1` against a `bigint` key planned as
  `bigint = text` — an operator that does not exist — and Postgres refused the
  statement. sqlx types each parameter from the Rust type it is given and
  always sends them in binary, so there is no "let the server work it out"
  escape hatch: values now go over as the type they are, and the variants this
  crate carries as strings (`numeric`, `uuid`, `json`, the temporal ones) are
  cast back in the statement itself. MySQL coerces on its own and never showed
  the bug, which is exactly why one engine's green run proves nothing.
- **`TINYINT(1)` is reported as `BOOLEAN`.** sqlx tells us `BOOLEAN` for a
  column MySQL stores as `TINYINT(1)`, and there is no way to distinguish "I
  meant true/false" from "I meant a one-digit integer". It decodes as a number:
  a `TINYINT(1)` can hold 7, and rendering that as `true` would be the editor
  lying about what is stored.

## Known limits

These are deliberate omissions in a starter, not oversights:

- **The text editor has no IME**, and vertical motion does not keep a goal
  column. The caret is drawn by splitting the line rather than by measuring
  glyphs — correct in a monospace font, but not a substitute for shaped text.
  Selection, undo, SQL highlighting, statement-aware run, and catalog
  autocomplete are implemented on top of that model.
- **Grid text is `div`-per-cell.** Fine at viewport scale, wrong long-term: the
  grid should be a custom `Element` painting shaped glyph runs.
- **Column widths are estimated**, not measured, from a 200-row sample. They
  can be dragged, and a dragged width is remembered by column name so it
  survives a reload — but it is not written to the session.
- **SQLite has no exact numeric type.** A `NUMERIC` column stores an IEEE
  double, so a price arrives as `Value::Float`. Reporting it as `Decimal`
  would claim an exactness the file does not have. sqlx also reports the
  declared type only for the spellings it knows, so a `JSON` column decodes as
  the text it is stored as; the structure pane still shows what was declared,
  because that comes from `pragma_table_info` rather than from the wire.
- **Composite foreign keys are not followable.** One cell holds one part of
  the key, and jumping on it would land on rows that merely share that part —
  so the introspection filters them out rather than offering a jump that lies.
- **The live tests cover the two engines' common ground**, not their corners:
  no `INTERVAL`, ranges, enums, `BIT`, spatial types, or generated columns. The
  decoders have arms for several of those; they are untested until a fixture
  exercises them.

# Agents

Ferrix is built by many agent sessions at once. Most of them own an area: a
stage, a subsystem, a port. Two roles span the whole fleet: the **product
owner** and the **certification consultant**. This file is for the session
the customer gives one of those roles, and for every other session, which
needs to know what to send them and when.

The rules every change follows are in [docs/CONVENTIONS.md](docs/CONVENTIONS.md),
and the gate table, the landing lock and the owners table are in
[docs/BACKLOG.md](docs/BACKLOG.md). This file adds the two roles and does not
repeat those rules.

Session names change on every restart, and a message to an old name reaches
nobody. `ListAgents` shows who is alive. The owners table in
`docs/BACKLOG.md` names who holds each role now.

---

## The customer

The customer owns Ferrix. The customer decides scope and priority, says
which session holds which role, and settles everything listed under
*Waiting on the customer* in `docs/BACKLOG.md`. A decision the customer gave
in one session is real only once it is written down as a dated entry under
*Decisions* in `docs/BACKLOG.md`. A decision that lives only in a message
between two sessions gets reverted by a third.

---

## The product owner

The customer names the session ("you are the po now"). The product owner
acts for the customer between the customer's words. It takes the fleet
coordinator's job from `docs/BACKLOG.md` (the landing order, the landing
lock, unblocking, pushes), plus the calls the customer has delegated to it.

### What it decides

* **Who works on what**, inside the customer's priority order: it assigns
  rows, gives ownerless rows owners, and asks a branch left unlanded for more
  than four hours for its plan.
* **When a row is done**, against the stage's exit criterion as written
  (`docs/BACKLOG.md`, *Calling a stage done*).
* **The landing order** when two landings contend, and whether a re-gate is
  owed after `main` moved.
* **Hardware use** (customer, 2026-09-26). No session boots the DK1, the
  Pixel 7 or any other device without the product owner's OK. A session asks
  with the exact list of addresses it will touch, checked against the device
  tree, and asks again for each phase that writes registers. Power-domain
  and PHY writes still need the customer's own word.
* **Disagreements between sessions**, inside one area or across two. A
  disagreement about scope goes to the customer.

### What it does not decide

* Scope and priority. A new subsystem, a dropped feature or a reordered
  priority list is the customer's call. The product owner puts the question,
  and it asks it with `AskUserQuestion` so the session shows as waiting.
* Anything under *Waiting on the customer*.
* Re-tasking the fleet after a wind-down. On 2026-09-15, "no one is doing
  something... why?" was a question about a release, not an order to restart
  six sessions.
* Permission prompts. A product-owner decision is a teammate's word. It never
  approves a tool permission on the customer's behalf.

### Taking the seat

1. Read `docs/BACKLOG.md`, `docs/roadmap/where-it-stands.md`, and the
   previous product owner's handover. The handover is on the gate host as
   `~/.local/share/ferrix/po-<date>/HANDOVER.md`, or in the previous PO's
   wind-down section on `main`. Also read the end of the fleet's landing log,
   `~/.local/share/ferrix/fleet/log`.
2. For every row the previous product owner left in flight, check `main`
   to see whether it landed. A row reported as "landed" may not have.
3. Run `ListAgents`, introduce yourself to every Ferrix session, and ask
   each one the area the customer gave it. Don't assume a map: on
   2026-09-13 all nine of a new PO's briefs went to the wrong areas.
4. Write the roster into the owners table in your first landing.

### Each round

* `land.sh status`: is the lock free, or is a hold older than fifteen
  minutes stale?
* *Red on `main`*: is any gate failing on `main` itself? A working `main`
  comes before every other row.
* Push `main` to `origin` when `origin/main..main` is not empty: fast-forward
  only, never forced, never with `--no-verify` (customer, 2026-09-27). Before
  pushing, `git fetch origin`, because the customer merges pull requests on
  GitHub.
* The root checkout is clean. Another session's edits there block every
  fast-forward. Ask the session that made them to move them to a worktree,
  and never reset them yourself.
* The gate host's disk (`df -h /`) and load. When every session goes quiet
  in the same minute, that is the account's usage limit, not a stall. Don't
  break locks or reassign work over that kind of silence.
* Every failure seen on a gate has a row with its log, filed the day it was
  seen.

### Running agents

Run about four agents at once, in priority order, and queue the rest. With
eight running on 2026-09-29, the usage limit stopped all of them within
twenty minutes, mid-gate. Brief each agent:

* Its own worktree, made by hand (`git worktree add -b <branch>
  .claude/worktrees/<name> main`), and its own `CARGO_TARGET_DIR` on the gate
  host. Two trees never share one.
* Gates and boots in the foreground, one architecture per call. A subagent
  is not woken by its own background task.
* A negative control for each new check, with the run that shows it fired.
* For a change that needs certification review, end the turn with a diff
  summary for the consultant, before `land.sh take`.
* Agents don't push. The product owner pushes after each landing.
* Ferrix is "Ferrix, a Rust operating system", never "a hobby OS", including
  in the context line of an agent's prompt (customer, 2026-09-28).

### Records

* Estimates are story points, never time (`docs/BACKLOG.md`, *Estimates*).
  Record each landing's estimate beside its spend.
* A milestone tag goes on `main` only after the whole matrix has passed on
  that commit. Its release notes go in `docs/RELEASES.md` before the tag,
  because the release workflow fails a tag without its section.
* At a wind-down, write a handover the next product owner can start from:
  `main`'s hash, the lock's state, each branch in flight with its worktree,
  what passed and its next step, and what waits on the customer. Put it on
  the gate host under `~/.local/share/ferrix/po-<date>/HANDOVER.md`, and put
  the parts that outlive the session in `docs/BACKLOG.md` and
  `docs/roadmap/where-it-stands.md`.

---

## The certification consultant

The customer names this session too ("you are the certification agent").
It is a standing role: when the fleet winds down, it winds down last.

Ferrix's certification targets are required, not optional (customer,
2026-10-01): Common Criteria EAL5+, DO-178C DAL C, IEC 62304 Class C and
EN 50716 SIL 2. The consultant's goal is to reach them. Its evidence is in
[docs/certification/](docs/certification/README.md), and its reviews keep
that evidence true while dozens of landings move the tree.

### Reviewing before the lock

[docs/CONVENTIONS.md](docs/CONVENTIONS.md), *Changes to the certified item
go through review*, says which changes need the consultant's review before
`land.sh take`, and what the review checks. The consultant:

* Answers each request in the turn it arrives, in one of three forms: **OK**,
  **OK if** a named condition is met, or **not yet**, with the reason.
* Gives a **design review before code** for a structural change: a new
  interface into the item, a moved boundary, a new obligation.
* Records each verdict where the next reader will look: the design
  document's "where it stands" section on `main`, and its own review ledger
  (below). A verdict that lives only in a message didn't happen.
* Checks a subagent's review claims in the code before recording them.

The rules its reviews enforce, kept from the first consultant's handover:

* A requirement is no broader than one check can prove. Split it rather than
  stretch a tag over it.
* A requirement states the correct behaviour, even while it is baselined.
  It never describes a defect.
* A finding closes when the build shows it closed. That means a check that
  fails without the fix, with the negative control's counts in the commit.
  Only where no emulator can show it does build evidence plus an argument
  stand in, and then the hardware run gets a `docs/BACKLOG.md` row.
* When item code moves, the bodies stay byte-identical and the boot lines
  stay identical on all four boots. Run `carry-coverage.py`, then
  `gen-coverage-justification.py --check`, after the final rebase.
* A landing that files a finding recounts the register when it lands.
  Finding numbers are reserved on confirmation, and the author is told the
  number.

### Keeping the evidence

* Findings found and closed, coverage entries, traceability, and updates to
  the threat and vulnerability analysis go into `docs/certification/` as
  small docs landings under the lock, batched rather than one per review.
* It raises the steps outside the repository that block every target with
  the customer as next actions: an accredited pre-assessment (`CLAIM.md` M2),
  a quality management system (F-28), independent reviewers (F-27), a
  position on AI-authored code (F-29) and a qualified toolchain (F-17).
* Its reviews are internal. They are **not independent verification** in
  the standards' sense, and are never presented as that.

### Watching `main`

At least once per round, the consultant lists the commits since its last
recorded verdict. It classifies their files against the `core` and `item`
rings of `tools/common/data/certification-item.json`, and checks their
messages for a recorded review. An item change that skipped review is
reviewed after the fact, and the verdict is recorded the same way
(da45a113 did this for four landings on 2026-10-01). It tells the author,
and tells the product owner if the skipped change is serious.

### What it does not do

* It writes no feature code, and it doesn't land another session's change.
* It runs no fleet-wide gates. It keeps one small worktree and target
  directory, and removes them between its landings.
* It doesn't decide what goes into the item, and it doesn't settle the
  order of the standards. `docs/certification/CLAIM.md`'s open decisions are
  the customer's.

### Its ledger

The review ledger is on the gate host:
`~/.local/share/ferrix/cert-consultant/reviews.md`, with each review, its
conditions and its outcome, and its open queue at the end. Beside it is
`HANDOVER.md`. A new consultant tells the product owner its session name,
works the open queue top to bottom, then audits `main` back to the last
recorded verdict.

---

## Every other session

* **Before a landing**, check your diff against
  `certification-item.json`'s rings. If it hits one, send the consultant the
  branch, the commit, what changes and how it is tested, and wait for its
  answer before `land.sh take`.
* **Before touching hardware**, get the product owner's OK with your list of
  addresses.
* **Report landings** to the product owner with the hash and the gates that
  ran, and ask it before changing your scope.
* **When everything you own is done**, report to the product owner. Then ask
  the customer in your own session, with `AskUserQuestion`: "<session> is
  done: <one line>. May I be stopped?" Ask a question only the customer can
  answer the same way, so a waiting session shows as waiting.

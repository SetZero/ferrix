# Ferrix marketing playbook

How Ferrix gets known: what we say, where, when, and how GitHub turns a
release into notifications. Written 2026-09-27, when the repository was public
with 0 stars, no description, no topics, no GitHub Releases and GitHub's default
link preview.

## 1. Positioning

**One sentence:** Ferrix is a from-scratch operating system in Rust that runs
`rustc` and builds its own image. A fleet of AI agents wrote it.

Two stories, told together, each carrying the other:

| Story | Who it hooks | Proof we can show |
|---|---|---|
| **A real OS in Rust.** Its own kernel, ring-3 drivers behind an IOMMU, btrfs, a libc, a shell and a Wayland desktop, running unmodified Linux programs | r/rust, r/osdev, Rust OSDev, Phoronix, HN systems readers | `test-rustc`, `test-selfhost`, Chrome on the desktop, the DK1 board, the Pixel 7 |
| **How far AI agents can go.** A dozen Claude sessions in parallel with a landing lock, gates and a certification consultant | HN at large, AI/agents readers, Simon Willison-type blogs, r/ClaudeAI | `docs/CONVENTIONS.md` (rules, and the incident behind each one), `docs/BACKLOG.md`, the commit history |

Lead with the result (the OS), and name the AI in the same breath. "Written by
AI" alone is common now, and so is "a Rust OS". What nobody else has is an OS
written by AI that hosts `rustc`.

**What we never do:** hide the AI; say "self-hosting" without qualification
(stage 20 is in progress; say "builds its own image"); claim daily-driver
readiness; post screenshots that are not real captures.

## 2. Assets (all in the tree)

| Asset | Where | Status |
|---|---|---|
| Logo, favicon, icons | `docs/brand/` | done |
| README banner (dark/light), badges, hero shot | `README.md` header | done |
| Social preview 1280×640 | `docs/brand/social-preview.png` | **upload by hand**: Settings → General → Social preview |
| Website with SEO (OG/Twitter cards, JSON-LD, FAQ schema, sitemap) | `website/`, deployed by `.github/workflows/pages.yml` | done; needs Pages switched on (the setup script does it) |
| Real screenshots | `docs/brand/screenshots/` | done; add a new one per release |
| Release pipeline | `.github/workflows/release.yml` + `scripts/gen/release-notes.py` | done |
| Community files | `CONTRIBUTING.md`, `SECURITY.md`, `CITATION.cff`, issue forms | done |
| Repository metadata (description, topics, homepage, Discussions) | `scripts/marketing/github-setup.sh` | **run it** |
| 60–90 s demo video | not made | **the highest-value missing asset**, see §6 |
| Prebuilt images on each release | not made | **the second highest**, see §6 |

## 3. GitHub, and how it notifies people

What GitHub actually does, so that we release in a way that reaches people:

- **A tag notifies nobody.** Only a *published GitHub Release* does. The four
  stage tags that exist today reached no one. `release.yml` now publishes a
  Release for every `v*` or `stage-*` tag pushed, with the notes from
  `docs/RELEASES.md`.
- **Who hears about a release:** watchers set to *All activity* or
  *Custom → Releases* get a notification and an e-mail. Stargazers get no
  notification, but the release appears in their GitHub home feed. So every
  release is a second impression on everyone who ever starred.
- **Editing a release does not notify again.** Deleting and re-creating one
  does, and people read that as spam. Get the notes right before the tag goes
  out: the workflow refuses a tag with no section in `docs/RELEASES.md`.
- **Pre-releases** don't become "Latest" on the repository page. Use them for
  release candidates if we ever have them, never for the headline release.
- **Trending** (github.com/trending, and /trending/rust) ranks by how fast stars
  arrive compared with the repository's usual pace. So put every launch post in
  **the same 24 hours**. The same stars spread over a week never trend.
  Trending drives more stars in turn, which is most of a launch's second day.
- **Search and topic pages** index the description and topics. The setup
  script sets 20 topics; `rust`, `operating-system`, `osdev` and `kernel` are
  the browsed ones.
- **Link previews** (HN, Reddit, Slack, Discord, X, Mastodon) use the social
  preview image. It is the single most-seen picture we have, so upload it
  before posting anywhere.
- **Pin the repository** on the owner's profile, and fill in the profile README
  with a line and a link.

### Release cadence

The fleet lands many times a day. That is too often to release: people unwatch
a noisy repository, and "Releases only" watchers are the ones we most want to
keep. The rhythm:

| Kind | When | Name | What it carries |
|---|---|---|---|
| **Milestone release** | about every **2 weeks**, on a **Tuesday around 15:00 UTC** (morning in the US, afternoon in Europe) | `v0.N.0`, titled with the milestone, e.g. `v0.2.0 — Chrome on the desktop` | a headline feature, a new screenshot or GIF, "try it" commands, known limits |
| **Launch release** | once, on launch day (§4) | `v0.1.0 — rustc on Ferrix` | the whole story so far |
| Stage tags | as today, when the PO verifies a stage | `stage-*` | published as a Release too, but only when no `v*` release goes out in the same week; otherwise fold it into the next `v*` notes |

Never publish more than one Release in a week. Skip a fortnight rather than
ship thin notes. A release whose headline is "fixes" costs watchers.

Numbering: move to `v0.N.0` now. People and tools understand versions, and
the "Latest" badge reads better as `v0.3.0` than as a stage name. The stage
names stay as release titles and tag messages.

### Release notes: what to write

The first screen of a release is what people read in their feed. Use this
shape (`docs/RELEASES.md` section, published verbatim):

```
## v0.2.0 — 2026-10-13, Chrome on the desktop

<one sentence: what you can do now that you could not before>

![screenshot or GIF](https://raw.githubusercontent.com/SetZero/ferrix/v0.2.0/docs/brand/screenshots/<file>.png)

**Try it:** `cargo xtask run-compositor --everything`

- **Headline 1.** What it is, and the gate that proves it.
- **Headline 2.** ...
- 3 to 6 bullets. The rest goes in "Also".

Also: <one paragraph of smaller things>

Known limits: <each with its backlog row>

Numbers: <commits since last release, lines of Rust, the gates added>
```

Use the absolute `raw.githubusercontent.com` URL for the image. A relative
path does not render in the notification e-mail.

## 4. Launch

### Before launch day (checklist)

- [ ] Run `sh scripts/marketing/github-setup.sh releases`.
- [ ] Upload `docs/brand/social-preview.png` as the social preview.
- [ ] Check https://setzero.github.io/ferrix/ loads, then paste the URL into
      https://www.opengraph.xyz/ and check the card.
- [ ] Pin the repository; add a profile README line.
- [ ] Record the demo video (§6) and put it on YouTube. Link it from the
      README and the site.
- [ ] Ideally, attach prebuilt x86-64 images to the release (§6), so "try it"
      is one `qemu-system-x86_64` line and not a full build.
- [ ] Have `cargo xtask run` checked on a *clean* machine (fresh Ubuntu VM and
      macOS): the first person who can't boot it posts that on HN.
- [ ] Write `docs/RELEASES.md`'s `v0.1.0` section.
- [ ] Decide who answers comments on launch day, for 6 hours after each post.

### Launch day: all inside 24 hours

Tuesday, Wednesday or Thursday. Times are UTC.

| Time | Where | Post |
|---|---|---|
| 13:00 | push tag `v0.1.0` | the Release goes out (feed of every future stargazer) |
| 13:30 | **Hacker News**, *Show HN* | §5.1. Post it yourself, from your own account. Answer every top-level comment. |
| 14:00 | **r/rust** | §5.2, with the "🦀 Project" flair |
| 14:30 | **r/osdev** | §5.3, where the technical bar is highest; lead with the kernel |
| 15:00 | Mastodon (hachyderm/fosstodon, `#rustlang #osdev`), Bluesky, X | §5.4, with the video or the hero screenshot |
| 16:00 | **r/programming** | the AI angle, §5.5 |
| same week | **This Week in Rust**: PR to `rust-lang/this-week-in-rust`, "Project/Tooling Updates", before Tuesday's cutoff | one line + link |
| same week | **This Month in Rust OSDev** (rust-osdev.com): PR to `rust-osdev/homepage` | a paragraph and a screenshot |
| same week | **Phoronix**: tip to Michael Larabel | three sentences and the video; he covers Rust kernels regularly |
| same week | **Lobsters**, if someone with an invite will post it (`rust`, `osdev`, `ai` tags); Lobsters dislikes self-promotion, so let someone else post | — |
| next week | awesome lists: PR to `rust-unofficial/awesome-rust` (Operating systems) and `jubalh/awesome-os` | one line each |

Don't post to more subreddits than these. Cross-posting the same text across
ten subreddits reads as spam and gets the account filtered.

### After launch

- Answer every issue within a day for the first two weeks. New stars watch
  how the first issues are treated.
- Label 5–10 issues `good first issue`. GitHub shows them on
  `/contribute`, and newcomers search them.
- Write one **technical deep-dive** blog post per month, on the site or
  dev.to: "How we ran rustc on a from-scratch kernel", "Ring-3 drivers behind
  an IOMMU in Rust", "What 12 AI agents need to share one git repository".
  Posts like these are what get shared long after launch.
- Give a talk. Submit to RustConf, EuroRust, RustNL, FOSDEM's Microkernel
  and Rust devrooms, and the OSDev community calls. The AI-fleet angle suits a
  general conference talk too.

## 5. Ready-to-post copy

Edit before posting. The numbers were true on 2026-09-27.

### 5.1 Show HN

**Title** (80 characters at most, no hype words; HN strips "!" and
superlatives):

> Show HN: Ferrix – a Rust OS, written by AI agents, that runs rustc and builds itself

**Text:**

> Ferrix is an operating system written from scratch in Rust for x86-64,
> AArch64 and ARMv7-A. It has its own kernel, userspace drivers behind an
> IOMMU, btrfs (read and write), a C library, a zsh-compatible shell and a
> Wayland compositor. On top of those it runs unmodified Linux programs: the
> rust-lang.org `rustc` and `cargo` with glibc's dynamic loader, git, curl
> and Google Chrome.
>
> The acceptance test was always "it compiles Rust". `cargo xtask test-rustc`
> boots it and compiles hello.rs through cc, collect2 and rust-lld over 350 MiB
> of shared libraries mapped from disk. `test-selfhost` runs `cargo xtask build`
> inside Ferrix and boots the image it produced.
>
> It was written by a fleet of Claude sessions working in parallel, with me as
> the product owner who decides scope. The first public commit is 16 days old.
> The process is all in the repository, including what went wrong:
> docs/CONVENTIONS.md has a rule for each incident, like the shared index that
> silently reverted another agent's commit. Every landing has to pass a gate
> that boots the whole system on three architectures.
>
> It is not a daily driver: no authentication yet, no namespaces. You can boot
> it with `cargo xtask run`. I'm happy to answer questions about the kernel or
> about running a dozen agents on one codebase.

Prepare answers before posting. These questions will come:

1. *Did a human review the code?* Answer truthfully, and the owner writes it,
   not an agent.
2. *How much did it cost?* Have the number, or say you won't share it.
3. *Is any of it copied from Linux or glibc?* Point to the clean-room rule:
   the reference sources are read, never copied, and glibc (LGPL) never at all.
4. *Does it boot on real hardware?* The DK1 board and the Pixel 7, but not a PC
   yet (stage 21).
5. *Why UEFI and no assembly?* `docs/ASSEMBLY.md`.

### 5.2 r/rust

**Title:** Ferrix: a from-scratch OS in Rust that runs rustc and cargo, and builds its own image (written by AI agents)

**Body:** the Show HN text, with the Rust-specific parts moved to the top:
850k lines of Rust with none vendored, 99.79% of the kernel in Rust with no
assembly at boot, ferrousli (a libc in Rust), zinc (a zsh in Rust running
oh-my-zsh), and uutils. End with what you'd like feedback on.

### 5.3 r/osdev

**Title:** Ferrix: Rust kernel on x86-64/AArch64/ARMv7-A with ring-3 drivers behind VT-d/SMMUv3, btrfs, and the Linux ABI. It runs rustc and Chrome.

**Body:** lead with the architecture: UEFI hand-off, EEVDF, W^X sweep,
grace periods, handles/channels/VMOs, IOMMU-isolated drivers that restart.
Then the boot self-checks with negative controls. Mention the AI fleet in one
honest paragraph, with the link to CONVENTIONS.md.

### 5.4 Mastodon / Bluesky / X (under 280 characters)

> Ferrix: an OS written from scratch in Rust, by a fleet of AI agents. It runs
> rustc, cargo, git and Chrome on its own kernel, drivers, btrfs and Wayland
> desktop, and it builds its own image.
> https://setzero.github.io/ferrix/ #rustlang #osdev

Attach the demo video, or else the hero screenshot. A post with media reaches
several times as many people.

### 5.5 r/programming (the AI angle)

**Title:** What happens when a dozen AI agents share one git repo for two weeks: an OS that runs rustc

Link the website, or better a blog post about the process: the landing lock,
the gates, and the incidents in CONVENTIONS.md. That is the post people outside
Rust will share.

## 6. The next two assets, in priority order

1. **A 60–90 second demo video**, no voice-over, captions only:
   - UEFI boot with the serial log scrolling (5 s);
   - the desktop appears, the terminal types `uname -a` (10 s);
   - btop, then Chrome opening a real site and playing a video with sound (20 s);
   - a terminal runs `rustc hello.rs && ./hello` (15 s);
   - ends on the logo, the URL and "written by AI agents, in Rust" (5 s).
   Record the QEMU window with OBS at 1080p60. Put it on YouTube (it is
   searchable and embeds on HN), link it from the README (a thumbnail image
   linking to the video) and the site, and cut a 15 s version for social posts.
2. **Prebuilt images on every release.** Extend `release.yml` to run
   `cargo xtask build --arch all --release` and attach the images, plus a
   `run.sh` with the QEMU line. "Download and boot in one command" roughly
   doubles how many people actually try an OS project. HN readers try it
   before they upvote it.
3. A **GIF** for the README, under 5 MB (GitHub autoplays GIFs, not videos,
   in READMEs): the desktop opening and Chrome loading.

## 7. SEO

- The site targets searches for `rust operating system`, `rust os`,
  `os written in rust`, `rust kernel`, `ai written operating system` and
  `ai agents build os`. They appear in the title, the description, the H1 and
  the FAQ, and the FAQ is also marked up as `FAQPage` data.
- Register the site in **Google Search Console** and **Bing Webmaster
  Tools**, and submit `sitemap.xml`. Verify with the HTML-tag method: add the
  `<meta name="google-site-verification">` line to `website/index.html`.
- The strongest ranking signal we can get is **links from other sites**:
  TWiR, Phoronix, awesome lists, Wikipedia's "List of operating systems"
  (only once third parties have written about it; Wikipedia requires
  independent sources). Every post links the website, not only GitHub.
- A custom domain (e.g. `ferrix.dev`) is worth buying before launch. Links
  made to `setzero.github.io/ferrix` keep working after a move, but ranking
  built up on that URL does not fully transfer. Add `website/CNAME` and update
  the canonical URLs.

## 8. Measuring

Weekly, into this file's log below:

- Stars and watchers (`gh api repos/SetZero/ferrix --jq '.stargazers_count, .subscribers_count'`).
- **Traffic**: Insights → Traffic keeps only 14 days, so copy views, clones
  and referrers weekly, or use `gh api repos/SetZero/ferrix/traffic/views`.
- Search Console: impressions and clicks for "rust operating system".
- Issues opened by people outside the project.

| Week | Stars | Watchers | Views (14 d) | Top referrer | Notes |
|---|---|---|---|---|---|
| 2026-09-27 | 0 | — | — | — | baseline, before any of this |

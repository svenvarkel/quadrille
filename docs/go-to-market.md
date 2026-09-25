# Quadrille — Go-to-market and monetization

*25 September 2026. Builds on [quadrille-analysis.md](quadrille-analysis.md); does not repeat its TAM work.*

## 1. Positioning

**One line:** *Edit the cell. Keep the file.*

**Category we claim:** the safe write path for tabular data, for agents and the people who check their work.
Not "a terminal spreadsheet" (l123, sc-im own that aesthetic) and not "a CSV viewer" (csvlens owns that).

Three proof points, in this order, everywhere:

1. **It never reformats your data.** Leading zeros, `SEPT2`, `1-3`, 19-digit IDs survive. Byte-identical unchanged saves.
2. **It opens files spreadsheet apps can't.** 1.29 GB / 1.8 M records scanned in 1.76 s with a 221 KB index.
3. **Agents get a contract.** JSON in/out, `--dry-run` diffs with before/after, refuses to clobber, nonzero exit on failure.

The hook that travels is #1. Everyone who has worked with data has an Excel-ate-my-zeros story, and the
science press has already done the awareness work for us (HGNC renamed ~27 human genes, `SEPT1`, `MARCH1`
etc., in 2020 because Excel kept turning them into dates). We don't need to explain the problem, only name it.

## 2. Audiences, ranked

| # | Who | Where they are | What they need to hear |
|---|---|---|---|
| 1 | People running coding agents (Claude Code, Codex, Cursor) over data files | MCP registries, agent plugin marketplaces, X/Bluesky, HN | "Give your agent a CSV/XLSX tool that can't mangle the file, and see its diff" |
| 2 | Data / analytics engineers with big CSVs | r/dataengineering, dbt Slack, Locally Optimistic, HN | "Fix row 1,204,331 without loading 5 GB into pandas" |
| 3 | Rust / terminal crowd | r/rust, This Week in Rust, ratatui showcase, terminaltrove | Early stars and contributors; credibility, not revenue |
| 4 | Regulated-data ops (mortgage/HMDA, finance, healthcare, public sector) | Direct, via Wasabi/Stablewood network | Audit trail and approval for agent edits. This is where money is. |

Audience 1 is new, growing and underserved. Everything in the launch favors it.

## 3. Distribution — get qd where agents already look

Agents install tools through `npx` / `uvx` / plugin manifests, not `cargo install`. That is the single
biggest lever and it is cheap:

- **crates.io** + **GitHub Releases** with prebuilt binaries (cargo-dist does this, including Homebrew tap and shell installer).
- **PyPI wheel** via maturin (`bin` bindings) → `uvx quadrille`. **npm** package with platform binaries → `npx quadrille`.
- **MCP**: once the server lands, list it in the official MCP Registry, Smithery, Glama, mcp.so, PulseMCP, and
  PR into awesome-mcp-servers. Ship a Claude Code plugin / skill that tells the agent *when* to use qd
  (any CSV/XLSX edit) — skills are what make an agent pick a tool without being asked.
- **Agent docs**: `llms.txt` on quadrille.dev and a short `AGENTS.md` snippet users can paste.

## 4. Launch sequence

1. **Before launch** (1–2 weeks): prebuilt binaries, `cargo install` works, MCP server merged, 20-second
   screen recording (VHS/asciinema) of: agent edits B7 → dry-run diff → human opens TUI → yellow cell → save → `cmp` says identical except the edit.
2. **Day 1 — Show HN**: "Show HN: Quadrille – a CSV/XLSX cell editor that can't reformat your data". Post Tuesday–Thursday ~15:00 Tallinn (US morning). Sven answers every comment for 6 hours; the limits section on the site is pre-emptive answers to the top 10 HN objections.
3. **Day 2–5**: r/rust (engineering angle: sparse index, quote-aware scanning), r/dataengineering (zeros angle), This Week in Rust submission, ratatui showcase, Lobsters (needs invite).
4. **Week 2**: MCP registries, one blog post per angle on quadrille.dev:
   - "Your spreadsheet app is editing your data" (SEO: *excel removes leading zeros csv*, *stop excel converting to date*)
   - "Opening a 5 GB CSV without loading it" (SEO: *edit large csv file*, *open huge csv mac*)
   - "Letting an agent edit spreadsheets safely" (the thesis post; the one that feeds audience 4)
5. **Ongoing**: comparison pages (qd vs csvlens, vs VisiData, vs Modern CSV). Honest tables rank and convert.

Success metric for the first 90 days is not stars. It is **weekly active installs via MCP/uvx/npx** —
add opt-in, anonymous `qd --version` ping or just watch registry/npm/PyPI download counts.

## 5. Monetization

The analysis is right that a paid terminal editor won't work. The tool stays free and permissive forever;
that is what makes it spread. Money comes from what sits **around** the tool, where a team — not a
developer — is the buyer.

### A. Quadrille Review — cell-level diffs in pull requests *(primary bet)*

Precedent: **ReviewNB** built a paid business on exactly one thing — rendering Jupyter notebook diffs in
GitHub PRs, per seat. CSV and XLSX in git today are just as unreadable: a one-cell change in an XLSX is a
binary blob diff; a CSV diff is a wall of red and green lines.

- GitHub/Bitbucket App: renders cell-level diffs for CSV/XLSX/ODS in PRs, comments on cells, flags
  "type drift" (a column that was all zero-padded strings now has integers).
- Built from the qd engine + an open **changeset format** (`schema.quadrille.dev/v1` — the domain was
  already planned for this). qd emits it, Review renders it, anyone can implement it.
- Free for public repos (distribution), ~$10–15 / user / month for private. Seat-based SaaS is the only
  model here with a server-side foothold, which the analysis correctly said open core lacks.

Requires: `qd diff a.csv b.csv` (JSON changeset) in the OSS tool first. That feature is also great
marketing on its own.

### B. Approval gate for agent edits *(enterprise, audience 4)*

Agent proposes a changeset (`--dry-run` output), a human approves in a web view / Slack / Linear, qd applies
it and writes a signed audit record: which agent, which prompt/session, which cells, before/after,
who approved. Sold to teams where "the agent changed a loan record" has to be explainable to a regulator.

- Priced per workspace, four to five figures a year. A handful of customers matters more than thousands of users.
- **Stablewood is customer zero**: HMDA data is in the test set already. Build it for internal use, then generalize.

### C. Wasabi consulting funnel *(certain, small, starts immediately)*

The site's teams CTA feeds Wasabi: "we'll wire agents into your data pipeline safely". Every serious
inbound lead is worth more than a year of sponsorships. This also validates A and B before building them.

### Not recommended

- Paid Pro TUI, license keys, feature gating in the CLI — kills spread, no precedent in this segment.
- Relicensing the core (BSL etc.) — already MIT/Apache, and a relicense would burn the community we need.
- Sponsorship as a plan — enable GitHub Sponsors, expect nothing.

## 6. Order of work

1. Ship MCP server (in progress) + prebuilt binaries + `uvx`/`npx` wrappers.
2. Deploy quadrille.dev (this branch: `site/`), record the demo, launch.
3. `qd diff` + changeset JSON schema published at `schema.quadrille.dev/v1`.
4. Watch which audience shows up. Agent users → build B with Stablewood. Git/data-team users → build A.
   Decide in ~8 weeks from real inbound, not from this document.

## 7. Launch checklist

- [ ] Repo public at the URL the site links (`github.com/svenvarkel/quadrille`; Cargo.toml agrees)
- [ ] `cargo install quadrille` works (crate published, version > 0.0.0)
- [ ] `hello@quadrille.dev` mailbox exists (site shows it)
- [ ] Real TUI screenshot / recording replaces the HTML mock in the hero, plus `og:image` (1200×630)
- [ ] DNS + TLS for quadrille.dev (static host: Cloudflare Pages / GitHub Pages; `.dev` is HSTS-preloaded)
- [ ] README "no MCP server" line updated when MCP lands; site tag "in development" removed

# Knowledge per layer: personal, group and org

Nov 19, 2026 · @Jesper

Every scope keeps its own three layers from the Context Layer Method: instructions always loaded, knowledge looked up, live data fetched. Personal agents and seats read the cells they are allowed to, and knowledge moves outward the same way agents do: by a deliberate commit.

## The grid

&#91;embedded content: three layers × three scopes\]

The rate of change decides the row and the audience decides the column. Vectors are needed in one row only, Knowledge; the row above is loaded whole and the row below is queried live.

## What lives in each cell

Nine cells: the method's three layers down, the three scopes across. The method's rules hold in every cell: dated, versioned, summary first, one document per topic, live data fetched and never stored.

| Layer | Personal | Group | Org |
| --- | --- | --- | --- |
| 1. Instructions (always loaded) | My premises (role, how I work), my routing card, my agent's mandate | The group's premises (what we own, our conventions), its routing card, its quorum rules | The org's premises (what we build, policies), its routing card |
| 2. Knowledge (looked up) | My notes, my agent's filed messages, my retros, private research | Decisions and their reasons, design docs, row write-ups, retros, incident notes, runbooks | Architecture, strategy, cross-group decisions, glossary |
| 3. Live (fetched) | My private rows, seats and asks; my mail and calendar | The group's board, git history, CI, the wsp event log | Org-wide wsp state, budgets, machines |

**Loading Layer 1 across scopes.** An agent loads the org's, its group's and its person's Layer 1, so each keeps the method's budget: about 600 words of premises and up to 15 routing rows per scope, about 1,800 words and 45 rows in all. Over budget, detail moves to Layer 2 with one line pointing to it.

**When scopes disagree,** the routing card says which scope is the authority for each kind of question, at trust 1.0, as the method does for sources:

| Kind of question | Authority | Example |
| --- | --- | --- |
| Policy, security, spend | Org | "May agents push to main?" |
| How this group works | Group | Branch naming, review rules, which ports |
| How I like to work | Personal | Tone, how often to be interrupted |
| Facts about current work | Layer 3 of the scope that owns the work | "Is wsp-153 done?" is answered from the board, never from notes |

A narrower scope may add to a wider one but never overrule it on the wider one's questions. A personal note saying "main is fine to push to" scores 0.05 against the org's policy.

## Curation

Agents do the gathering and tidying; people approve what becomes shared and what becomes always-loaded. Knowledge moves along two axes, each by an explicit step.

**Up the layers (the method's moves), within one scope:**

| Move | Who proposes | Who approves |
| --- | --- | --- |
| Live to knowledge | Any agent that sees something worth keeping: a finished row's write-up, a decided proposal, an incident | Nobody in a personal scope; in a group, it is filed as a draft until a member accepts or edits it |
| Knowledge to instructions | The personal agent or a group's curator agent, when a finding recurs (three retros saying the same thing) | The scope's owner: you, a group lead, an org admin |
| Down again | The curator, when a Layer 1 premise starts changing weekly | The same owner |

**Out to wider scopes, by commit,** exactly as agents move in the multiplayer design:

1. `wsp knowledge commit <doc> --to <group>` proposes a personal document to the group. It is checked against the mandate (nothing private rides along) and against the group's existing documents (a near-duplicate becomes an edit, not a second copy).
2. A group member accepts it, or a quorum of agents does if the group's rules allow it for that kind.
3. The group's copy is the new authority; the personal one is marked superseded, with a link.

**Tending.** Each scope gets a curator: a role the personal agent plays for you, and a seat per group and for the org. It works on the method's rhythm: Layer 1 reviewed each quarter, Layer 2 tidied each month, the routing card checked after every new source. It looks for undated or stale documents, duplicates, documents nobody retrieved in 90 days, and sources with no routing row. Its findings go to the owner's Needs you as a short list, never as silent edits to shared documents.

## Retrieval

No single kind of RAG fits all nine cells. Match the technique to the shape of the knowledge, and keep vectors for the one cell they suit best: Layer 2 prose.

| Knowledge | Technique | Why |
| --- | --- | --- |
| Layer 1, any scope | No retrieval: always loaded | Small by budget; prompt caching keeps it cheap |
| A Layer 2 cell small enough to fit | Load it whole, cached | Anthropic's advice: under about 200,000 tokens (roughly 500 pages), skip retrieval and put it all in the prompt |
| Layer 2 prose: decisions, docs, notes, retros | Hybrid search: vectors plus keyword (BM25), merged by reciprocal rank fusion, then a reranker | Vectors find meaning, keywords find exact names like `wsp-153` or `COMPOUND_RUN_DIR`; each misses what the other catches |
| The same, at index time | Contextual retrieval: a model writes one or two lines placing each chunk in its document before it is embedded and indexed | Reported to cut retrieval failures by 49%, and by 67% with reranking (Anthropic, Sep 2024) |
| Layer 3: rows, seats, asks, budgets | Structured queries through the wsp API, as tools | The method's rule: live data is fetched, never embedded; an embedded board is stale the moment it is written |
| Code | Search the tree (grep, the language server, git log), not embeddings | Code changes per commit; a stale embedding of it misleads |
| Who decided what, and why | The store's own links: row to decision to proposal to answers | Already a graph; graph RAG adds little until the links get dense |

**Agentic, not one-shot.** Agents get retrieval as tools (`knowledge.search`, `knowledge.read`, `wsp` queries) and call them as often as they need, guided by the routing card, instead of having chunks stuffed into the prompt once.

**Every hit carries its metadata:** scope, layer, date, version, trust, superseded or not. Retrieval filters on it before ranking, and the answer cites it, so the method's conflict rules (highest trust wins, newer date wins, say so when no row covers it) can be applied by the agent.

## Storage

Recommended: keep the documents as files in git, one repo per scope, and treat every index as rebuildable from them. Start with SQLite beside wsp's store, and move to Postgres with pgvector when the multiplayer design moves its store to a server database.

**The truth is files.** Each scope's Layers 1 and 2 are Markdown in a git repo: dated, versioned and reviewed like code, readable without any tool. This matches wsp, where files stay the truth and the store mirrors them. The vector index is only a cache: a new embedding model or chunking scheme means a rebuild, never a migration.

| Option | Fits | Watch out for |
| --- | --- | --- |
| SQLite with sqlite-vec plus FTS5 | Three engineers: one file per scope next to wsp's SQLite store, no extra service | sqlite-vec is pre-v1 and warns of breaking changes, so pin it; the index is rebuildable, so that risk is small |
| Postgres with pgvector plus full-text search | Many groups: HNSW or IVFFlat indexes, filtering with iterative index scans (0.8+), hybrid search merged by reciprocal rank fusion; current release 0.8.7 | A database to run; filters on approximate indexes need partitioning or partial indexes by scope |
| A dedicated vector database (Qdrant, LanceDB and others) | Very large corpora or heavy filtering | A second store to secure, back up and keep in step with workspaces |
| A hosted retrieval service | Fast start | Org and private knowledge leaving the org's machines |

The path follows the multiplayer phases. Phase 2, when every record gets its workspace, gives each scope its own index. Phase 5, when the store moves to a server database, is the moment to move indexes to pgvector in the same database, with one partition per workspace.

## Privacy and scope in the index

One index per scope, never one shared index with a filter: a filter is a bug away from a leak, a separate index is not.

- **Reads follow workspace rights.** An agent's `knowledge.search` runs over the indexes its principal may read: its person's private one, its groups' and the org's. The server picks the indexes from the token, not from the request.
- **Embeddings are content.** A vector can be turned back into a close paraphrase of its text, so a private index gets the same protection as the private documents.
- **Personal agents answer from the shareable.** When Ed's agent answers your question, it searches only what Ed's mandate shares; his private index stays his.
- **Where it runs.** Embeddings and contextual summaries call a model. Private and group content goes only to providers the org has approved for it; a local embedding model is the option when it may not leave the org's machines at all.
- **Forgetting.** Deleting or superseding a document removes its chunks in the same commit, and a scope can be re-indexed from its repo at any time to prove nothing is left behind.

## Provenance and audit trail

Every piece of knowledge carries where it came from, who vouched for it and how much to trust it, and every use of it is logged. An agent's answer can then be traced back to the exact text it relied on, and that text back to its sources and its approver.

**Provenance travels with the document and with every chunk cut from it.** It is written once, when the document is created or changed, and never edited afterwards; a correction is a new version.

| Field | What it records | Example |
| --- | --- | --- |
| Origin | Who or what wrote it: a person, or an agent with its seat, model and session | `seat cpd-40, claude-opus-5-5, session …` |
| Method | How it was made: written by hand, summarised, extracted, translated | Summarised from three retros |
| Derived from | The sources it rests on, each as a link with a date and a content hash | `wsp-152` at rev 4; commit `640dbc5`; a web page fetched 2026-11-19, sha256 … |
| Approval | Who accepted it into the scope, when, and in which commit | Jesper, 2026-11-19, `knowledge@a1b2c3` |
| Lineage | What it supersedes, and what has superseded it | Supersedes the personal note of 2026-11-12 |
| Scope and layer | The cell it lives in | Group, Layer 2 |

**Trust is derived, not asserted.** Nobody types a trust number. It starts from the kind of origin and moves with evidence:

| Origin | Starting trust | Can it reach Layer 1? |
| --- | --- | --- |
| Written by a person and approved by the scope's owner | High | Yes, by the owner |
| Written by an agent, then accepted or edited by a person | Medium-high | Yes, after a person promotes it |
| Written by an agent, not yet reviewed (a draft) | Low; retrieval labels it "draft" | No |
| From outside the org (web pages, vendor docs, pasted text) | Low; treated as data, never as instructions | Only rewritten by a person |

Trust then rises when sources still resolve and their hashes still match, when independent sources agree, and when answers that cited it held up. It falls with age against how fast the topic changes, with each recorded contradiction, and when an answer that relied on it was later corrected. The scope's authority from the routing card still applies on top: a narrower scope never overrules a wider one on the wider one's questions.

**The audit trail is append-only and has two halves.**

- **Git history** holds every version of the documents: author, co-author (the agent), date and reviewer, per commit.
- **The knowledge log** holds every action on them as an event: proposed, accepted, edited, promoted, superseded, forgotten, and every retrieval, naming the agent, the row it was working on, and the document versions returned. It lives beside the wsp event log and uses the same principals.

**Answers cite versions, not just documents.** A citation is `doc@version#chunk`, so the trail survives later edits. "Why did the agent do this?" then walks backwards: the row, the answer, the cited chunks at their versions, their sources, and the person who approved each one.

**Forgetting keeps the trail, not the text.** Deleting a document removes its content and chunks, and leaves a tombstone in the log: its id, hash, scope, who deleted it, when and why. The record that something existed and was used stays checkable without keeping private text.

**Poisoning is the risk this guards against.** Knowledge an agent fetched from outside could carry instructions or plain errors. Low starting trust, the "draft" label, the rule that outside text never reaches Layer 1 without a person rewriting it, and the curator's monthly check for sources that no longer resolve together keep one bad page from spreading into always-loaded instructions.

## Proof of concept

Run the method's own test, once per scope, before building any vector store: it shows whether the layers and routing cards help, and only then whether search is the bottleneck.

1. **Files only.** Each of the three engineers writes personal Layer 1; the group writes its Layer 1 and moves its first decisions and retros into a Layer 2 repo. Agents read Layer 2 by loading it whole or by grep. Run the method's ten questions per person, plus ten group questions.
2. **Add hybrid search** over Layer 2 only when a scope outgrows loading it whole, or when questions fail for lack of recall rather than wrong routing. Ask the same questions again.
3. **Add contextual retrieval and a reranker** if the failures that remain are retrieval misses.
4. **Add cross-scope tests:** a personal note contradicting a group convention, a group decision superseded by an org policy, and a question about Ed's private work asked by Jesper's agent. They pass when the authority wins, the conflict is named, and nothing private is quoted.
5. **Add a trace test:** pick three answers and walk each back through the log to its cited versions, their sources and their approvers. It passes when every step resolves, an agent-written draft is labelled as one wherever it appears, and a deleted document shows as a tombstone, not as a gap.

## Open questions

- [ ] Which embedding model: hosted, or local so private content never leaves the org's machines?
- [ ] Is a group's Layer 2 one repo per group, or a folder in the group's main repo next to its code?
- [ ] Who may promote to the org's Layer 1: org admins only, or a quorum of group leads?
- [ ] Should live sources outside wsp (mail, calendar, issue trackers) come in through MCP connectors per scope?
- [ ] Does the trust scale hold across scopes, or does each scope need its own scale?
- [ ] How long does the knowledge log keep retrieval events: as long as the rows they served, or a fixed period?
- [ ] Should approvals and commits to a group's or the org's knowledge be signed, so an approver can't be impersonated?

## Sources

- [The Context Layer Method](https://claude.ai/artifact/U1NweCXNxcP1mxFYcPeg81): the three layers, routing card, trust scale and test this builds on
- [Introducing Contextual Retrieval](https://www.anthropic.com/news/contextual-retrieval), Anthropic, 19 Sep 2024: failure-rate reductions and the 200,000-token threshold
- [sqlite-vec](https://github.com/asg017/sqlite-vec): vector search for SQLite, pre-v1
- [pgvector](https://github.com/pgvector/pgvector): HNSW and IVFFlat, iterative index scans, hybrid search with Postgres full-text search
- wsp multiplayer: team capabilities on the server, and Personal agents: acting as you, the companion designs

## Breakdown

Plan `wsp-156`, 2026-11-19. It changes no code. The rows are sub-tasks of wsp-156, though it is their lists' membership that runs them: the parent link stays until a person detaches them (`wsp mv <id> --parent none`), and until then `wsp done wsp-156` needs `--force`. The lists are at status **init**: none starts until a person sets it planned.

Reviewed with the other two breakdowns by wsp-157 (2026-11-20). Planned at low priority: `kl1-files`, `kl2-search`, `kl3-contextual`, `kl5-provenance` and `kl6-tending`; each starts by itself once its `after` is complete. At init, for the person to plan: the six measure lists, each after the person's step in front of it; `kl7-pgvector`, below; and `kl4-scopes`, which rests on `mp3-commit` and `pa2-mandate` through wsp-274, waits that the run does not act on across lists (src/after.rs). Because `kl2-search` and `kl3-contextual` are planned, a no-go from `kl1-measure` or `kl2-measure` has to set the list it rules on back to init: wsp-202 and wsp-210 do that before their reviews. `kl7-pgvector` is the same case as `pa2-mandate`: its list waits only on `mp5-scale`, while wsp-298 also rests on wsp-206 in `kl2-search`, which may never run and which the run does not hold it on, and the person confirms the Postgres isolation reading first. Planned, it would start as soon as `mp5-scale` completes without either wait.

### The lists

Each phase is two lists. The first builds, and it ends at a step only the person can do: installing the trunk, and what the measure needs after that (documents written, providers set, a month gone by). The second is the phase's measure, one row, and it waits on the first. So no list stands at a person's step with its governor seated (the handbook's rule, wsp-091), as `s2-state-core` and `s2-state-core-2` are cut. The next phase waits on the measure.

| list | spec | rows | after | ends at |
|---|---|---|---|---|
| `kl1-files` | Proof of concept step 1: files only. A repo per scope, Layer 1 in every brief, Layer 2 read whole or by grep, reads scoped from the token, the question harness | wsp-193, wsp-195 to wsp-201 | `pa1-tell` | the person installs the trunk, runs `wsp knowledge init` for every scope, and the engineers and the group write Layer 1 and their first Layer 2 documents |
| `kl1-measure` | Step 1's gate: ten questions per engineer and ten for the group | wsp-202 | `kl1-files` | "Phase 1 measured": go or no-go for search |
| `kl2-search` | Step 2: hybrid search. One index per scope, FTS5 plus vectors, reciprocal rank fusion, embedders a scope approved | wsp-203 to wsp-209 | `kl1-measure` | the person installs the trunk, sets each scope's providers, and reindexes every scope with `--prove` |
| `kl2-measure` | Step 2's gate: the same questions again | wsp-210 | `kl2-search` | "Phase 2 measured": go or no-go for step 3 |
| `kl3-contextual` | Step 3: contextual retrieval and a reranker | wsp-211 to wsp-213 | `kl2-measure` | the person installs the trunk and approves the context and rerank providers |
| `kl3-measure` | Step 3's gate: the same questions under three configurations | wsp-214 | `kl3-contextual` | "Phase 3 measured": the configuration each scope keeps |
| `kl4-scopes` | Step 4: authority and conflict, drafts, commit outward, the mandate on reads, and the three cross-scope cases as a sandbox test | wsp-274 to wsp-282 | `kl3-measure` | the person installs the trunk |
| `kl4-measure` | Step 4's gate: the three cross-scope cases asked of real agents | wsp-301 | `kl4-scopes` | "Phase 4 measured": three verdicts |
| `kl5-provenance` | Step 5: provenance, derived trust, logged retrievals, tombstones, `wsp knowledge trace` | wsp-283 to wsp-290 | `kl4-measure` | the person installs the trunk, and the fleet works on it until real rows hold retrievals |
| `kl5-measure` | Step 5's gate: the trace test | wsp-291 | `kl5-provenance` | "Phase 5 measured": the spec's three conditions, met or not |
| `kl6-tending` | Curation the five steps leave out: a curator per scope, moves into and out of Layer 1, a quorum's accept | wsp-292 to wsp-296, wsp-299 | `kl5-measure` | the person installs the trunk |
| `kl6-measure` | A month's tending, read by each owner | wsp-297 | `kl6-tending`, and a month after its install | "Tending measured" |
| `kl7-pgvector` | Storage's last step: the indexes follow the store into Postgres | wsp-298 | `mp5-scale` | the five index cases on the new backend |

That is 48 rows in 13 lists. A measure list is set planned once the person's step in front of it is done, and not before: at init it holds no seat. `kl6-measure` also waits a month, either at init or planned and held (`wsp worklist hold kl6-measure --until <install + 30d> --for "a month's tending on the installed trunk"`).

The measures that ask questions (wsp-202, wsp-210, wsp-214, wsp-301) run the harness of wsp-201, which starts one seat at a time, so none loads the machine (wsp-086). wsp-291 and wsp-297 start no seat.

A list has one `after`, so what else a list rests on is in its overview ("Before group 1") and on its first row:
- `kl1-files` rests on multiplayer phase 2 and personal agents phase 1. `pa1-tell` is itself after `mp2-workspaces`, and wsp-193 rests on wsp-180 and wsp-224, the last rows of the two.
- `kl4-scopes` rests on `mp3-commit` (stewards, the rights table) and `pa2-mandate` (the mandate). wsp-274 rests on wsp-192 and wsp-232.
- `kl5-provenance` rests on the real agents' verdicts: wsp-283 and wsp-284 rest on wsp-301.
- `kl6-tending`'s group 5 rests on `pa4-quorum`. wsp-299 rests on wsp-246. If `pa4-quorum` is not complete when group 4 lands, the list does not wait: wsp-299 moves out to a list of its own, after `pa4-quorum`.
- `kl7-pgvector` rests on `kl2-search` for the index contract. wsp-298 rests on wsp-206.

**kl2 and kl3 are conditional.** The spec adds search "only when a scope outgrows loading it whole, or when questions fail for lack of recall", and contextual retrieval and a reranker only "if the failures that remain are retrieval misses". The measure before each says go or no-go: `kl1-measure` for `kl2-search`, and `kl2-measure` for `kl3-contextual`. `kl4-scopes` needs only `kl1-files` among the knowledge lists, and every row in it works on a scope with no index. It is `after kl3-measure`. If a measure says no-go, leave the lists it rules on at init and point kl4 at that measure: `wsp worklist after kl4-scopes kl1-measure` when search is not built, `wsp worklist after kl4-scopes kl2-measure` when search is built and step 3 is not.

Every group runs on `claude opus high`, except three rows on `claude fable high`, where the spec leaves the design open: wsp-193 (the document and routing-card format), wsp-275 (the authority and conflict rule) and wsp-285 (the trust function).

### What is reused, and from where

Nothing parallel to docs/state-core.md or docs/agent-manager.md is built.

| the spec's | is |
|---|---|
| a scope | multiplayer phase 2's workspace: an engineer's private one, a group's, and the org's |
| "the same principals" | state-core 2.1's `Principal`: a person, a seat, the instance, and personal agents phase 1's `agent:<person>`. A curator is a seat. No principal is added |
| the knowledge log | a document's stream in the store's one event table (`Subject::Doc`), written by `apply` with its idempotency key. No second log |
| documents as files | state-core 2.5, option C: prose people write stays Markdown under git. The store holds where a scope's repo is and the commit it is at |
| the indexer, the curator's rhythm, the source check | once-only work as state-core 8.1 has it: a lease (`index:<scope>`, `curate:<scope>`, `sweep:knowledge:<scope>`), a fenced write, and a key on the effect |
| "one index per scope, never a filter" | an `Index` trait in domain terms with no scope parameter, and one set of conformance cases every backend passes, as state-core 2.3 does for the store. pgvector later is a second backend |
| who may set, accept, promote | multiplayer's roles and rights table, in `wsp-machines` |
| the mandate | personal agents phase 2's record and its one function, which gains a knowledge document as a thing it rules on |
| a quorum of agents | personal agents phase 4's proposals, with `knowledge` as one more kind. No vote is counted here |
| findings to Needs you | an ask to the scope's owner, in multiplayer phase 2's bands |
| a model call | a program a scope has approved, spoken to over stdin and stdout, and run as a manager job where that is practical (agent-manager 3.2). wsp links no model and no HTTP client, so docs/components.md's dependency gate stands |
| contract changes | each by a row of its own (R5): wsp-195, wsp-203, wsp-211, wsp-274, wsp-283, wsp-292 |

### Assumptions to confirm

These shaped the rows. None is the spec's own words.
1. A scope is multiplayer phase 2's workspace or the org. mp2 gives the org no workspace (wsp-168 makes `private:<engineer>` and `group:<group>` only), so wsp-193 adds the org scope in wsp-knowledge alone, with the same text form, and the contract's `WorkspaceId` is not changed (wsp-157's review).
2. A scope's repo of record is a checkout on the store's host. People edit in a clone and push, and agents read through the server, which scopes every read from the token.
3. Agents reach knowledge through `wsp knowledge` verbs over the API's `knowledge.*` verbs. The spec's `knowledge.search` and `knowledge.read` "as tools" are those. No MCP face is built in these lists.
4. A private scope's documents that the mandate shares are a second index of their own (`<scope>.shared`), so a personal agent answering for someone else never opens the private one.
5. Until `kl4-scopes` lands the mandate rule, a personal agent answering another principal's message gets `not_found` for its person's private scope. It fails closed from `kl1-files` on.
6. A document's proposal (`DocProposal`: a document offered to a scope) is not personal agents phase 4's proposal (a question put to a group's agents). `kl6-tending` joins the two.

### The spec's open questions

They are not settled here. Each has a recommended answer in a note on wsp-156, and no list needs one of them before it can be planned.

| # | question | recommended | needed before |
|---|---|---|---|
| 1 | Which embedding model | local for private scopes, hosted where the org approves it | the step `kl2-search` ends at, where the person sets each scope's providers. No row waits on it: they test with fake providers |
| 2 | One repo per group, or a folder | a repo per scope; a folder is a valid root | nothing |
| 3 | Who promotes to the org's Layer 1 | org admins only | `kl6-tending` group 2 |
| 4 | Outside live sources through MCP | yes, per scope, in a list of its own later | nothing here |
| 5 | One trust scale or one per scope | one scale, authority applied on top | `kl5-provenance` group 2 |
| 6 | How long retrieval events are kept | as long as events are kept (state-core Q7) | nothing |
| 7 | Signed approvals | not in these lists | nothing |

### Found while planning

- **Forgetting and git.** The spec keeps "the trail, not the text", and git keeps every earlier version. wsp-288 builds `wsp knowledge forget --purge`, which rewrites the scope repo's history, and leaves it off unless asked for. Whether it is the default is the person's.
- **An edited provenance cannot be prevented**, since people push. wsp-286 does not serve a version whose provenance was edited: the version before stays the answer, and the owner gets an ask.
- **Two repos are not one transaction.** Accepting a commit writes the group's repo and then supersedes the personal document. wsp-280 keys the second step by the proposal and has the daemon finish one that was cut short.
- **The query is content too.** wsp-209 embeds a query only with a provider approved for every scope searched, and falls back to keyword for a scope with none.
- **One citation type.** `Citation` and its text form `doc@version#chunk` are wsp-knowledge's alone (wsp-204), which links wsp-base and serde_json and not the contract. The contract carries the same three fields in an event as `Cited`, with no text form (wsp-283), and the server copies one into the other where both are linked (wsp-287).
- **The spec's grid is "&#91;embedded content\]" in this copy.** The table under "What lives in each cell" carries the same nine cells.

# Client data-version pair catalogue

This catalogue maps the **client-side** versions in `elements.data` and
`tasks.data` to the Perfect World release families they have been observed
with. It is used only as a fallback identity hint when the executable
fingerprint, `config/element/version.sw`, and a plausible PE game-version
resource do not already identify the build.

It is deliberately not a compatibility table and does not select a parser
layout. A release label here means that the pair is useful evidence about the
client's data lineage; it does not mean every feature in that client is
supported by this viewer.

## What is compared

- `elements.data`: the version is the low 16 bits of the dword at offset 0.
  The high 16 bits must be the expected family marker `0x3000`.
- `tasks.data`: the dword at offset 0 must be `0x93858361`, and the version
  is the dword at offset 4.
- Both files must be present and readable. A match on only one file is not
  sufficient.
- `task_npc.data`, `dyn_tasks.data`, `aipolicy.data`, timestamps, PCK
  container revisions, and filenames are not substituted for either value.

The executable's known fingerprint and valid self-declared release version
have priority. The pair catalogue is consulted only after those sources fail
to provide a game version. This is important for patched clients and mixed
regional installations.

## Pair catalogue

`elements/tasks` below means the actual two header revisions, not a guessed
numeric sequence. Regions are shown where the source distinguishes them.

| elements/tasks | Client release(s) | Evidence / context | Scanner result |
|---|---|---|---|
| `6 / 52` | 1.1.3 | Client set based on the Beimei 1.1.6 package; client task revision is v52 (server task revision is v53) | Exact |
| `7 / 55` | 1.2.6 | Brazilian official client inventory | Exact |
| `8 / 55` | 1.2.8 | W2I/Chinese release inventory | Exact |
| `10 / 55` | 1.3.1 | Russian client inventory; also measured in this repository's matrix | Exact |
| `12 / 56` | 1.3.4, 1.3.6, 1.3.8 | Russian/Chinese release lists and three distinct 1.3.6 client data sets | **Ambiguous** |
| `27 / 75` | 1.4.1, 1.4.2 | Modified Brazilian 1.4.1 client based on 1.4.2 data; W2I 1.4.2; this repository measured the pair in a 1.4.2 client | **Ambiguous** |
| `29 / 78` | 1.4.1, 1.4.2 | Modified PWI-based 1.4.1 client; official Russian 1.4.2 client | **Ambiguous** |
| `60 / 93` | 1.4.4 | Russian 1.4.4 client | Exact |
| `63 / 101` | 1.4.5 | Russian 1.4.5 Underworld client | Exact |
| `69 / 103` | 1.4.5 | Russian 1.4.5 Reflections client | Exact |
| `70 / 105` | 1.4.4, 1.4.5, 1.4.6 | This repository's measured 1.4.4 corpus; W2I 1.4.5; Russian 1.4.6 | **Ambiguous** |
| `80 / 105` | 1.4.7 | PWI 1.4.7 client | Exact |
| `84 / 106` | 1.4.6 | W2I/Chinese 1.4.6 client | Exact |
| `85 / 108` | 1.4.8 | PWI and Russian 1.4.8 inventories | Exact |
| `88 / 111` | 1.5.0 | Russian 1.5.0 client | Exact |
| `99 / 116` | 1.5.0 | W2I/Chinese 1.5.0 client | Exact |
| `101 / 118` | 1.5.1 | Russian and Chinese 1.5.1 client data sets | Exact |
| `123 / 120` | 1.5.2 | W2I/Chinese 1.5.2 client | Exact |
| `127 / 121` | 1.5.2 | Independently measured client in this repository's matrix | Exact |
| `145 / 124` | 1.5.3 | PWI 1.5.3 client | Exact |
| `153 / 127` | 1.5.3 | W2I/Chinese 1.5.3 client | Exact |
| `156 / 128` | 1.5.5 | Russian/Beimei 1.5.5 client data set | Exact |
| `181 / 135` | 1.5.5 | W2I/Chinese 1.5.5 client | Exact |
| `190 / 140` | 1.5.6 | PWI 1.5.6 client | Exact |
| `204 / 146` | 1.5.7 | Russian 1.5.7 client | Exact |
| `230 / 156` | 1.6.2 | W2I/Chinese 1.6.2 client | Exact |
| `233 / 157` | 1.6.2 | PWI and Russian 1.6.2 inventories | Exact |
| `244 / 162` | 1.6.4 | PWI 1.6.4 client | Exact |
| `253 / 167` | 1.6.5 | Russian 1.6.5 client | Exact |
| `267 / 176` | 1.6.6 | Chinese 1.6.6 client | Exact |
| `386 / 191` | 1.7.6 | Russian 1.7.6 build 4311 client | Exact |

The full matrix used by the project to record local file measurements is
[Cross-version reading matrix](cross-version-matrix.md). The release-list
sources below provide additional regional client pairs. Where an exact pair
is present in both sources but the release names disagree, the scanner keeps
the candidate list instead of taking the most convenient label.

## The important collision: 1.3.6

The pair `elements.data v12 / tasks.data v56` is **not a 1.3.6 fingerprint**.
Published inventories show it in 1.3.4 and 1.3.8 clients too. More importantly,
the multi-client archive documents three separate client data sets called
1.3.6, all carrying the same pair:

| 1.3.6 data set | Based on | elements records | tasks/quests |
|---|---|---:|---:|
| v1 | Chinese 1.3.4, updated with `ec_patch_84-101.cup` | 25,591 | 3,941 |
| v2 | Chinese 1.3.8 converted data | 26,140 | 4,116 |
| v3 | PWI 1.3.9 converted data | 26,878 | 4,441 |

The original post explicitly says it is a collection of client files and that 1.3.4/1.3.6 servers do not exist; these should be read as community-labelled client data sets, not proof of official server releases. Those counts are content-level evidence, not header fields. The viewer does
not currently parse the v12 elements tables or v56 task records deeply enough
to reproduce these counts. The safe result for `12/56` is therefore
`unknown game version (ambiguous data pair; candidates: 1.3.4 / 1.3.6 / 1.3.8)`.
A later content-fingerprint pass could count the relevant records or compare
stable file hashes and narrow the candidates. It would be unsafe to label every
`12/56` client as 1.3.6 now.

The same approach is taken for `27/75`, `29/78`, and `70/105`: the files are genuine
client pairs, but more than one marketed release is documented with those
headers. A known executable fingerprint or its own release marker still wins.

## Known gaps and intentionally excluded evidence

- **Elements-only mappings are not enough.** Some lists associate elements
  revision v6 with more than one early release, but do not establish the
  matching client-side `tasks.data` revision for each. The scanner does not
  fill in a tasks version from the server database or another file.
- **Server-side pairs are not client pairs.** Several archives explicitly list
  different server-side elements/tasks numbers from the client's files. The
  catalogue includes the client columns only.
- **1.7.6 is regional.** The `386/191` entry is the documented Russian build
  4311. It must not be assumed to identify the separate PWI English build.
- **1.8.9 is not auto-mapped.** A community work-in-progress post reports the **server-side** data
  pair `elements v620 / tasks v247` for a 1.8.9 build. It does not establish
  that this is a verified client-side pair, so it is not mapped.
- **1.8.0 and 1.8.4 remain release-marker driven.** Their explicit
  `version.sw` mappings are more authoritative than trying to infer those
  versions from an unverified data pair.
- A missing pair is **unknown**, not proof that a release does not exist.
  Regional variants, hotfixes, conversions, and server-specific changes can
  introduce pairs not represented here.

## Sources and evidence

1. RaGEZONE, [All versions of Perfect World servers](https://forum.ragezone.com/threads/release-all-versions-of-perfect-world-servers.1180355/),
   including page 3's W2I/Chinese, PWI, and Russian client pair lists. The
   thread is community-maintained and some posts require sign-in; values are
   treated as reported inventory data, not as official publisher certification.
2. RaGEZONE, [Multiple PW Servers in One VM](https://forum.ragezone.com/threads/multiple-pw-servers-in-one-vm.1213091/),
   which gives 1.3.4 and the three 1.3.6 variants with their element/task
   revisions and record counts, and documents the modified 1.4.1 client.
3. MMORPG-Devs, [1.5.5 (156) multi-server PW 1.1.3–1.5.5](https://mmorpg-devs.ru/threads/Мультисервер-pw-1-1-3-1-5-5-15-в-1-в-1-одном-virtualbox-vmware-образе.21112/),
   which separates client-side from server-side pairs across Russian and
   Beimei builds, including 1.4.4 through 1.5.5.
4. RaGEZONE, [How to find Server version on the client side?](https://forum.ragezone.com/threads/how-to-find-server-version-on-the-client-side.1248159/),
   which explains the regional divergence of elements revisions. This source
   is used as supporting context, not as a complete two-file mapping.
5. RaGEZONE, [PWI Patch Versions](https://forum.ragezone.com/threads/pwi-patch-versions.916588/),
   which distinguishes patch numbers, marketing client versions, and the
   independent elements/tasks data revisions.
6. Project-local [Cross-version reading matrix](cross-version-matrix.md),
   which records pairs read from the staged 1.3.1, 1.3.6, 1.4.2, 1.4.4, and
   1.5.2 client corpora.
7. RaGEZONE, [1.7.6 server + client, page 6](https://forum.ragezone.com/threads/1-7-6-server-client.1246869/page-6),
   which explicitly lists the Russian 1.7.6 build 4311 client as
   `elements.data v386 / tasks.data v191`. The [offline server/client project](https://github.com/vladyslav-tmf/perfect-world-offline-server)
   independently warns that the PWI English build uses different data.
8. RaGEZONE, [Oriental PW 1.8.9 development thread](https://forum.ragezone.com/threads/release-wip-pw-1-7-6-source-1-8-0-now-running-the-1-8-9-client-pw-oriental-build-10469-%E2%80%94-reaper-azure-dragon-log-in.1275720/),
   a community WIP reference only; its reported `620/247` pair is not enabled
   in the scanner.

These sources describe community and regional client inventories, not a single
canonical global release history. The detector intentionally favours a
transparent ambiguous/unknown result over a confident but unsupported
marketing-version guess.

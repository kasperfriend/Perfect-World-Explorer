# Live logging: how to test, and how to report what you saw

Everything here is in **Settings ▸ LOGGING**, the fifth tab. It is off by default
and does nothing until a box is ticked, so it is safe to leave alone.

## Where the files go

`<explorer folder>/pwe-logs/` — next to the viewer's executable, **not** inside
the client install.

| How you start the viewer | Where the logs land |
| --- | --- |
| Shipped build, `Perfect-World-Explorer.exe` in `D:\PWE\` | `D:\PWE\pwe-logs\` |
| `cargo run` / `target\debug\…` | the checkout root, `target\debug` is unwrapped |
| Old settings that pointed inside the client | moved to the explorer root on first start |

**The client install is refused outright.** A saved folder from an older build, a
typed one, or a default that lands there all resolve to the explorer's own folder
instead, and the page says so under the path. If the executable itself is sitting
in the game folder, the logs go to `%LOCALAPPDATA%\Perfect-World-Explorer\pwe-logs`
(`~/.perfect-world-explorer/pwe-logs` elsewhere) rather than into the install.

The page shows the resolved path and what is in it (`7 files, 2.4 MB`) under the
folder box. **Open** shows it in the file manager, **Explorer folder** puts the
box back to the default, and **Clear files** deletes the `.log` files so the next
capture starts from an empty folder.

Three kinds of file:

* **`<channel>.log`** — one per ticked channel, `[+12.345]` on every line, rotated
  to `.log.1` at 8 MiB;
* **`session.log`** — always written: the run header (client root, explorer root,
  version, mask), every capture started, every folder change, every **F8** mark,
  every summary written. It is the file that explains the others a week later;
* **`summary.log`** — written by the **Summary** button: every channel file merged
  into one digest, **one line per subject** (a root and an element id, or the
  shape of a structural line) with how many lines that subject wrote. Its header
  names the **binary that wrote it** — full path, size and write time, from
  `std::env::current_exe` — so a report cannot be mistaken for a newer build than
  the one it came from. Read that line first: if it predates the fix the report is
  about, nothing else in the file matters. Six files
  and a million lines become a page that names every root that drew. This is the
  file to attach when the tail is all one busy channel.

```text
## effect-uv -- 4142 lines, 74896 throttled, 204800 bytes on disk
   6 subjects
   (x1204) [ 118.307] gfx/场景/水门_瀑布.gfx id=100 tile=1 grid=100x100 ...
   [ 119.835] gfx/场景/花瓣分飞.gfx id=121 #197 tex=gfx/textures/物体/flowers2.dds ...
```

`Summary` replaces the previous `summary.log` each time, so the newest click is
the one to send. `Copy report` stays the short version: the switches, the counts,
and a tail capped at 20 lines per channel so one root cannot bury the five boxes
that were ticked.

## Groups, one click each

The **GROUPS** row ticks a whole family of channels and leaves everything else
off. The names are general on purpose — they say what a line *is*, so the same
group answers "why is this shaft the wrong way round" and "why is this sheet
frozen", because both questions are about the same records.

| Group | Ticks |
| --- | --- |
| **Placement** | roots, element table, element frames and basis, culling, clock |
| **Cards** | card corners, UV window, decals, textures, colour and alpha folds, and the clock |
| **Sub-elements** | particles, trails, meshes, lights, ground drape, and the clock |
| **Scene** | the `world.ecwld` records, effect draws, the model queue, problems |
| **Everything** | every channel that can write |

A group **replaces** the mask instead of adding to it, so a file is about one
question. **Cards** and **Sub-elements** also tick the clock, because a UV window
and a turning ring are both functions of it: a capture that says what a window was
without saying whether it moved answers half the question. Channels are grouped under the same four headings on the Effects page —
**Where it is**, **Cards**, **Sub-elements**, **Diagnosis** — and each heading,
like each page, has its own tick box. Every bulk switch touches only the channels
that can write, so it never leaves empty files behind.

## Nothing floods a file, and nothing is missed either

Four limits, and all of them are counted so nothing disappears quietly:

* **one line per thing, not per frame.** A card, a texture, a model, a light and a
  root that was culled are each written the first time they appear, then at most
  once every few seconds — so a file is the *complete list* of what drew: every
  root, every element, every card, every texture, once each. That is why a
  watermark of five sheet roots and the four hundred splash cards around them
  all make it into the same file instead of six of them winning a race;
* an **unchanged line is collapsed** — a line identical to the one before it is
  counted, not written, and the run of duplicates ends with one line saying
  `(240 identical lines collapsed)`;
* a **budget of ~200 lines a second per channel**, with a burst of 4,000 so one
  load can still dump a whole census;
* the per-frame channels keep their own **throttle**, which the budget sits on top
  of.

The row next to each tick box shows what happened: `1,204 lines`,
`1,204 lines, 12 budgeted, 3,880 repeats collapsed`. Lines and repeats together
are the truth: *40 lines, 3,900 repeats collapsed* is a complete list of forty
things seen a hundred times each, not a channel that stopped working.

## F8 marks the spot

Press **F8** anywhere in the world and a line is written to `session.log` and to
every channel that is on, whatever the throttle says:

```
---- MARK camera (1528.0, 602.5, 1032.3) yaw 0.000 pitch -0.150 time 12.00 ----
```

Press it once when the artefact comes into view and once before you leave. The
pair of marks is the window to read; everything between them is the evidence,
with a position and a timestamp instead of a description.

## The five steps

1. **Click the group** that covers the question.
2. **Fly to the spot, get the artefact on screen, press F8.**
3. **Wait a few seconds** — two or three frames of a waterfall, one full turn of
   a ring — then **press F8 again**.
4. **Click Summary** and **attach `summary.log`**, with **Copy report** pasted in
   the message. The report carries the build, the client root, the folder, the
   live mask, every switch that is on, what each of the 36 channels wrote (lines
   and throttled lines), and a tail with at most 20 lines per channel. The
   summary names every root and element that drew, once each, with a count — so
   "the wrong root drew nothing" and "the right root drew the wrong thing" read
   differently at a glance.
5. **Open a channel file only when the summary is not enough** — attach the one
   file, not the folder. `Clear files` between defects.

(`Copy tail` copies the live tail alone, when the header is already in the
report.)

## Reading a channel that produced nothing

| What you see | What it means |
| --- | --- |
| *no hook in this build yet* next to the box | there is no call site for it yet: ticking it writes nothing, on purpose |
| `on, nothing yet` | wired, ticked, and genuinely silent so far — it has not been reached |
| a count that climbs, an empty tail | the tab is scrolled away, or the lines rolled off; the file has them |
| `N throttled` | the channel limits itself per millisecond; the lines were dropped on purpose, not lost to a bug |
| the file exists but is empty | the header was written and nothing followed — same as `on, nothing yet` |

## What each open report needs

| Report | Group | What settles it |
| --- | --- | --- |
| Sun rays point at random | **Placement** | `effect-frames` (keypoint + basis columns) and `effect-cards` (corners) against the `dir`/`up` pair `scene-effects` prints for the same placement |
| Archosaur beams are both horizontal | **Placement** | `effect-frames` for both bases; `effect-clock` for whether the second placement starts late |
| Waterfall does not move | **Cards** | `effect-uv` against `effect-clock`: the scroll lives in the sheet roots, so look for the root that owns the running water — its `grid`/`speed`/`tile` says whether the window advances, and `(N identical lines collapsed)` on a UV line says it does not |
| Crater is pure blue | **Cards** | `scene-effects` names the placement, `effect-cards` the root, `effect-textures` whether the texture resolved, `effect-alpha` the fold |
| Wraithgate ring spins fully | **Sub-elements** | `effect-trails` for the ribbon, `effect-uv` for the scroll, `effect-clock` for the turn |
| Shroom lights sit too low | **Sub-elements** | `effect-cards` for each card's world Y, `effect-ground` for whether a drape moved it |

## If a capture is impossible

Say so instead of waiting: a channel marked *no hook in this build yet* cannot
produce a file, and a report that asks for one is a report that cannot be
answered. The rest of the header — build, client root, mask, folder — is still
worth pasting, because it rules out the wrong build and the wrong folder first.

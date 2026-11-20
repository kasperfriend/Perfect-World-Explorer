# Librarian

Librarian browses the Perfect World 1.4.2 v27 `elements.data` installed in the
selected client directory. Enable it from **Escape → Modules → Librarian**.
It reads client archives without modifying them; missing optional archives do
not prevent the item index from opening.

Select a category and type, search by name or ID, and select a result for its
icon, decoded attributes, available add-on pools, and linked equipment sets.
Add items to the Comparison tab to align their known numeric attributes. The
first column in each category is its baseline; arrows reorder columns within the
category, × removes a column, and
the item name returns to its details. Add-on weights are the authored pool
weights, not necessarily final in-game probabilities. Comparison columns have
explicit Left, Right and Remove controls.

## Effects

Search the installed client's `gfx.pck` and loose `element/gfx/` tree by path.
Select a file to load it and its dependencies. **Animation** plays the whole
composition or isolates a named element while preserving its binding targets.
Drag the 3D view to orbit, adjust Zoom, or scrub **Animation cycle** to pause
on a frame. The slider includes start offsets, repeated plays and their gaps.
Forever holds and indefinite loops have a labelled inspection interval instead
of a fictitious end time. Going backwards replays the simulation from its seed.

Details show file size, group, GFX version, keypoints, playback speed and live
geometry/particle counts. The preview uses the shared GFX player and renderer;
it does not play sounds, distort the world or light surrounding scenery.
Unsupported containers remain browsable and missing dependencies are reported.
No client assets are copied into the module or needed at build time.

## NPCs

The highlighted tab row carries **All Items**, **Comparison**, **NPCs** and **Effects**. It
reads the same `elements.data`: the 2,838 `NPC_ESSENCE` records and the 4,208
`MONSTER_ESSENCE` records, switched between with the NPCs/Monsters buttons.
Search matches names and IDs, and selecting a template opens its details.

An NPC shows its authored `.ecm`, type, greeting, refresh time, tax rate,
whether it is territorial, and — when the record names one — the monster whose
attributes replace its own, with a link to that monster. Monsters show level,
life, defence, the five magic defences, attack and armor, damage,
attack/aggro/sight ranges, speeds, experience, money and their behaviour
enumerations (aggressive code, ranged or close-range attacks, habitat, stance,
on-death outcome, stealth levels). Every service an NPC offers is resolved
through its own service table, so the shop, task, craft and teleport tables are
named from client data instead of being described generically; the
`combined_services` field is expanded with the client's own bit meanings.
Monster drop tables list their item templates with the authored probabilities
and the per-roll count distribution, and every drop links to the item's details.
Monster skills and the type's add-on rolls are shown the same way. Habitat
labels are the client's own `inhabit_type` strings (`Ground`, `Water`, `Air`,
`Ground+Water`, `Ground+Air`, `Water+Sky`, `Ground+Water+Air`), and the tax
field is already a percentage in the file rather than a fraction.

The 3D preview hands the record's own `.ecm` to the same renderer the item tab
uses, so orbit, pan, zoom, flip, turntable and the action list all work. It
starts on the standing scene action the client requests for a placed NPC
(`站立`, with the client's own `休闲` fallback), and rigs with no such action keep
their first clip.

`NPC_ESSENCE` and `MONSTER_ESSENCE` carry no icon field: these templates have no
2D artwork of their own, and the pet egg a monster is captured as belongs to the
pet system rather than to the creature. The tab therefore shows the rig and
nothing else instead of inventing a picture, and the monster's pet egg is listed
as the attribute it is.

**Add to comparison** on a template drops it into the shared **Comparison** tab,
under an *NPCs and monsters* section beside the item columns. Adding a creature
does not switch tabs. Columns are aligned on the attributes both records carry,
so an NPC and a monster compare on what they share and an item never appears in
the creature section. The baseline and best-value highlighting work the same way
as for items.

Switch to **3D model** for an offscreen ECM/SMD preview, or a class/gender
floating equipment mesh for armor and fashion. Select the class and gender
variant used to load the equipment mesh; no character body is displayed.
Left-drag to orbit, right-drag to move, and use the Zoom slider. Use Flip
vertically for inverted assets; turntable and animation controls are below the
image. Weapons are shown in bind pose without animation controls. Dyeable fashion
has an approximate color-tint picker, not an exact dye mask simulation. Common
action names are translated to English; unrecognized actions are numbered rather
than guessed. Models and textures require the corresponding client archives or
matching loose files. Missing resources and unsupported models are reported in
the preview instead of being substituted.
For effect-only models with an embedded GFX skin model, the preview can show
the first embedded model in bind pose (requires `gfx.pck`). It omits GFX
transforms, animation, particles, decals and other effects; effect-only models
without a supported embedded model cannot be previewed.
Skeletons with unsupported reflection or constraint controllers use an
explicitly labelled approximate pose.

Client files and localized data vary. Unknown item fields and add-on types are
not guessed: unrecognized add-ons are shown as raw values. The comparison shows
only attributes currently decoded for each item type, and does not calculate a
character's final combat stats or apply randomized add-ons.

For optional tests against an installed client, set `PW_LIBRARIAN_ELEMENTS` to
its `data/elements.data` path and `PW_LIBRARIAN_ELEMENT_ROOT` to its element
folder before running `cargo test --locked --bin perfect-world-explorer`.
Set `PW_LIBRARIAN_GPU=1` as well to render real weapon and worn-armor previews
and read back their pixels on a headless GPU.

The supplied `pw142` client indexes 13,483 unique items across 43 nonempty
item tables, exceeding the 12,922-item reference. Counts may differ between
client builds; the browser does not discard extra records to force a count.

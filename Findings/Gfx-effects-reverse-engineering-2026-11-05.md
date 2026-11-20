# GFX effect container: grammar, runtime player and corpus verification

Three new modules carry the retail `.gfx` effect format end to end: `gfx_file` (the container grammar), `gfx_player` (the clock, controller, particle, ribbon, bolt and child runtime that turns a parsed root into textured quads) and `gfx_effect_gpu` (a per-blend-bucket wgpu receiver). The walker's persistent character effects now try the verified static subset first (`gfx_decal`) and hand every root it rejects to the player, so animated weapon and body effects play instead of being dropped. Addresses, offsets and instruction sequences below were read out of the inspected `elementclient.exe`; corpus counts were measured over every `.gfx` member of the retail `gfx.pck` (5,459 members, 10,104 in the archive overall).

## Container grammar

Text, GBK encoded, CRLF terminated, `Key: value` per line with the first colon as the separator. `MOXTVersion` gates the rest: versions above 79 or below 24 are rejected, because 24 is the client's own gate for the effect container and 79 is the newest version in the corpus. The root header carries `DedaultScale` (sic), `PlaySpeed`, `DefaultAlpha`, `Raytrace`, `FaceToViewer` (version 39+), `FadeByDist`/`FadeStart`/`FadeEnd`, two `Vec` lines for the AABB centre and half extents, `UseAABB` (57+), `ShakeCam`, then zero or more root controllers, then `NoChangeDir`/`2DRender`/`2DBackLayer` (50+) and `PhysExist` (64+), then `GFXELEMENTCOUNT` and the elements. Every element shares 26 optional, version-gated keys plus a class-specific block selected by `GFXELEMENTID`, then its affector list, then `StartTime` and its keypoints. 146 distinct keys occur across the corpus and all of them are named in `KNOWN_KEYS`; an unnamed key is a hard error rather than a silent skip, and the element-125 sphere emitter additionally writes a keypoint-shaped generation sequence before its affectors.

Element IDs in the corpus, with counts: 100 sprite 3,537; 101 trail/quad 5,244; 102 mesh-facing quad 83; 110 ribbon 2,956; 120–125 particle emitters 1,315/320/40/9,869/1,739/382; 130 light 546; 140 wave surface 1,116; 150/151/152 lightning 734/11/156; 160 model 447; 170 sound 491; 180 79; 190 curve 208; 200 nested child 1,530; 210/211 215/1. Sprites, particles, ribbons, waves and bolts produce quads; lights belong to `gfx_light`, models to the model queue, sounds to the audio path and curves are counted but not drawn, so the player reports them in `Stats` instead of approximating them.

## Native evidence

| Address | What it establishes |
| --- | --- |
| `0x5b33f3`, `0x5b3700`, `0x788960` | ECWLD effect record (60 bytes) and the placement matrix path a world effect is parented through |
| `0x5b18a1`, `0x5b1a00`, `0x70bd20`, `0x7b2520` | day/night-only roots rejected at 0.5, at most one new root admitted per frame, duplicate roots updated in place, hook resolution |
| `0x71a1f1`, `0x71a217` | particle tick converts the integer millisecond delta (`fild; fmul 0.001`) and scales it by the root's live `PlaySpeed` copy at root+0x1ec |
| `0x71a247` | lifetime: `age += dt; if (age >= ttl) unlink`, age at node+0x00, TTL at node+0x04 |
| `0x715150` | the whole initialiser of the `.data` global `0x9c1318`: `flds 0x8edaa8` (10.0), `fmuls 0x8edab0` (0.8333333), `fstps 0x9c1318`, `ret` — so the emission-LOD numerator constant is **8.3333333**, a compile-time product rather than runtime state, and the LOD thresholds at `0x8edaa8`/`0x8edaac` are **10.0** and **40.0** |
| `0x70ba00` | the `Priority` level-of-detail gate: `0x6fa670(arg)` first (`0x70ba0e`), the byte at element+0x12e next (`0x70ba23`), then `Priority` at element+0xdc against `+0x134` of the object `0x41a3c0` returns (`0x70ba36`–`0x70ba44`, signed greater wins), and element+0x50's negative case is the only `return false` on that path (`0x70ba4f`) — and the jump target `0x70bd0b` is `mov $0x1,%al; pop %esi; add $0xac,%esp; ret $0x4`, so `Priority > detail` **returns true** and skips the visibility test `0x6fe120` on element+0xa0 (`0x70ba91` is the only `return false` after it) together with the distance scalar `0x6fe060` builds from element+0x58 plus element+0x90 into `[esp+0x24]`, which `0x70bae7` passes on under root+0x2a2's gate |
| `0x715b50`, `0x715c09` | emission, and the quota rule `count = min(count, quota - live)` with −1 unlimited (live at +0x1a8, quota at +0x1a4) |
| `0x716240` | affector order: the node is copied into a 0x3c-byte temp struct (`+0x00` position, `+0x0c` colour, `+0x10` scale, `+0x24` rotation, `+0x28` velocity, `+0x34` age), every affector's `Update` (vtable slot 1) runs in list order, the result is copied back |
| `0x7165b0` | particle integration: `speed += ParAcc*dt; d = (speed_old + speed_new)*0.5*dt; pos += d*dir`, with the displacement mirrored into node+0x50 and a second accumulator at node+0x5c for `Acc` along the authored `AccDir` (settings+0x1c) |
| `0x6eaf60`, `0x6eb230` | controllers 100 and 101: `v = Vel + Acc*age`, `dspl = (2*v + Acc*dt) * dt * 0.5` — the client adds `v` to itself, adds `Acc*dt`, multiplies by `dt` and by the 0.5 it loads from `0x8d5894`. 100 adds `dspl*Dir` to both the position (temp+0x00) and the velocity accumulator (temp+0x28); 101 adds `dspl` to temp+0x24, the rotation slot, which is why the corpus writes `Vel: 1.745329` (100°/s in radians). Both skip the step while `age < StartTime`, and both compare `EndTime` against the 0.0 at `0x8d58a0`, so *any* negative end means forever — the corpus only ever writes −1 (74,490 of 76,496 controller records) |
| `0x6eb400`, `0x6eb860`, `0x7b6fd0` | controllers 102/103 feed the same `dspl` to the axis-angle helper and rotate about `Axis` through `Pos` |
| `0x6ebba0`, `0x6ebd0a` | controller 104: `target = Pos + velocity_accumulator`, `n = normalize(position - target)`, `position += n*dspl`; `Vel == -1` disables it, `dspl == -1` snaps onto the target |
| `0x6ebf70`, `0x6ebfab`, `0x6ec00b`, `0x6ec02a`, `0x6ec03f` | controller 105 unpacks the ARGB dword at temp+0x0c into red, green, blue, alpha **in that order**, so `ColorDelta`'s four integers are R, G, B, A and the corpus's `ColorDelta: 0, 0, 0, -1000` is an alpha fade. Per channel: `acc = delta*dt + remainder; i = (int)acc; remainder = acc - i; channel = clamp(channel + i, 0, 255)`, with one remainder per channel at ctrl+0x24…+0x30 that is cleared when the byte saturates, then the four bytes are repacked |
| `0x6ecad0` | controller 106: `scale += ScaleChage*dt`, clamped low then high to the authored bounds (defaults 0.01 and 9999) |
| `0x6ec240` | controller 108's colour ramp is the client's own 128-entry PC24 table, which this repository already reproduces in `gfx_color_track` |
| `0x41a430` | ground snapping of spawned particles |

Sprite geometry is not re-derived here: it is copied from `gfx_decal.rs:405–500`, the one sprite path in this repository that was written against the client's decal calls. Dimensions are doubled and split at `OrgPt` (`w0 = Width*2*OrgPt[0]`, `w1 = Width*2 − w0`, `h0 = Height*2*OrgPt[1]`, `h1 = Height*2 − h0`), the *top* extent is `h1` and the bottom is `h0`, both axes are multiplied by the keypoint scale unless `NoScale` suppresses it, alpha is folded in 0..255 units and truncated, and a quad below 5/255 is dropped. Depth testing is `version < 76 || ZEnable`. 704 corpus `OrgPt` lines are off centre, so the split is not a theoretical case.

## The client's vector library

The emission routines are written against a small vector and quaternion library.
Eight of its routines are decoded completely; each is short enough to read end to
end, and together they account for every helper the surface and basis code calls.

| Address | Convention | Semantics |
| --- | --- | --- |
| `0x401ca0` | thiscall, `ret $0xc` | `Vec3::new(x, y, z)`: stores the three float arguments to `this+0/+4/+8`, returns `this` in `eax` |
| `0x401cc0` | thiscall, `ret $0x4` | copy-assign: copies 12 bytes from the argument into `this` |
| `0x401d10` | thiscall, `ret $0x4` | `+=`: adds the argument's three floats into `this` |
| `0x633700` | thiscall, `ret $0x4` | `*= scalar`: multiplies all three components of `this` by the one float argument |
| `0x4093d0` | cdecl, three pointers | `cross(out, a, b)`: `out.x = a.y*b.z - a.z*b.y`, `out.y = a.z*b.x - a.x*b.z`, `out.z = a.x*b.y - a.y*b.x` |
| `0x4071f0` | cdecl, three pointers | `add(out, a, b)` |
| `0x407220` | cdecl, three pointers | `sub(out, a, b)` |
| `0x401ce0` | thiscall, `ret $0x4` | `*= scalar`, the second spelling of `0x633700`; both appear in the same routine (`0x729670` uses `0x633700` for the rotation terms and `0x401ce0` for its third row) |
| `0x634420` | thiscall | rotate a vector by a quaternion: called with the axis and the quaternion at `0x729972`, its result scaled by `0x401ce0` and copied by `0x401cc0` into a basis row |
| `0x7b6fd0` | cdecl, `(axis, angle, out)` | axis-angle to quaternion: `out+0..0xc = axis * sin(angle/2)`, `out+0xc = cos(angle/2)`, with the halving by `0.5` at `0x8eff98` |
| `0x7b6f10` | cdecl, three floats | Euler angles to quaternion, the `sin(a/2)cos(b/2)…` expansion of three successive rotations, each argument halved by `0x8eff98`; called at `0x724313` and `0x71b479` |
| `0x7b7010` | cdecl, four plain pointers/values, caller cleans 16 bytes (`add $0x10,%esp` at `0x71600e`) | quaternion **slerp** `(out, a, b, t)`: `dot = a·b` at `0x7b701d`; `dot < 0` negates `b` and the sign so the short arc is taken; `|dot| > 0.75` (`0x8f0aec`) blends linearly as `(1−t)a + sign·t·b`; otherwise `θ = acos(dot)` and the coefficients are `sin((1−t)θ)/sinθ`, `sign·sin(tθ)/sinθ`. Stores at `0x7b7100`, `ret` at `0x7b7124`. Its first argument is the output, not an input |
| `0x70a320` | thiscall, `ret $0x10` | quaternion setter `set(this, w, x, y, z)`: arg1 → `this+0xc`, arg2 → `+0x00`, arg3 → `+0x04`, arg4 → `+0x08` (`0x70a320`–`0x70a33d`), so the argument order is scalar-first while the memory layout stays `(x, y, z, w)` |

Two conventions matter when reading a caller. The x87 helpers reserve argument
slots with `push %reg` / `fstps (%esp)`, so the frame shifts by four bytes per
push and the same textual offset names different slots at different points;
`0x727f00` reads what looks like three separate values at `0x74/0x7c/0x80(%esp)`
that are all one argument. And a call's argument count follows from its `ret`,
not from the pushes around it: `ret $0x4` means exactly one.

Rotation by a quaternion is *usually* spelled out inline, though it does exist as a
helper (`0x634420`, which `0x729670`'s third basis row uses). Given `q = (v, w)`,
the inline callers compute `u + 2w(v x u) + 2(v x (v x u))` with one `0x4093d0` per
cross, one `0x633700` per scaling and two `0x4071f0` adds — which is Rodrigues'
formula, and it is how the surface samplers and two of the three unbound basis rows
are written.

Reading those call sites needs one more convention. The second `0x4071f0` of a
rotation is preceded by only two pushes and followed by no stack adjustment,
because the first call's four pushes were cleaned by an `add $0xc` that
deliberately leaves the outermost argument in place for the next call to consume.
Counting pushes per call therefore misreads the argument list; the balanced form
is `G2 + A + B`, the rotation itself.

### Per-class birth flow

The four classes that honour `IsSurface` do not share a birth flow, and the
difference decides whether a surface particle keeps the emission cone.

| Class | Divert | Surface sampler | Volume sampler | After both |
| --- | --- | --- | --- | --- |
| 120 | `0x72729c` | inline at `0x7272aa` | `0x727230` | — |
| 121 | `0x7277c1` | `0x727800` | `0x727ab0` | `vtable[1](&node+0x44)` |
| 123 | `0x727ec2` | `0x727f00` | `0x728270` | `vtable[2]` (TTL) only, at `0x727ed8` |
| 124 | `0x729ba1` | `0x729be0` | `0x729df0` | `vtable[1](&node+0x44)` at `0x729bc1`, then `vtable[2]` |

So id 123's sampler is the last word on the direction — it writes the negated
sample to node+0x44 itself and nothing overwrites it — while ids 121 and 124 have
the direction slot called afterwards and their samplers write no direction at
all. Ids 122 and 125 never read emitter`+0x84`.

**Id 120's surface block does not sample a surface.** `0x727230` is the whole
birth routine, with the node as its argument (`ebp = [esp+0x94]` at `0x727238`) and
the divert inline: `0x72729c` reads `IsSurface` at emitter`+0x84` and `je 0x7276a7`
sends the volume case away. Before the divert the prologue fills node`+0x0c` from
vtable[3] (`0x7237f0`, the colour slot, at `0x72724d`), node`+0x6c` from vtable[5]
(`0x727257`), node`+0x08` from vtable[6] (`0x727261`), node`+0x60` from
emitter`+0x14` — the `Speed` — at `0x727267`, and node`+0x34…+0x40` from the four
runtime globals `0x9c0318`, `0x9c031c`, `0x9c0320`, `0x9c0324` at
`0x727270`–`0x727290`, which is the particle's initial orientation before anything
composes into it.

The surface branch spends **one** `rand()` draw (`0x7272aa`), scales it by
2π/32767 (`0x8e6f14`) at `0x7272bd` and turns it into a quaternion about the global
Z axis `0x9c0338` through `0x7b6fd0` at `0x7272d1`. `0x7272ec`–`0x727376` then
rotate the global X axis `0x9c0358` by that quaternion — the same inline Rodrigues
chain as everywhere else, `0x4093d0` twice, `0x633700` with the doubled w and with
`2.0`, `0x4071f0` twice — leaving the spun perpendicular at `[esp+0x74]`.
`0x72737b` computes `π/2 − Angle` (`0x8ee228` minus emitter`+0x10`) and `0x727395`
feeds it to `0x7b6fd0` with that spun axis, giving a quaternion stored at
`[esp+0x20]`. `0x72739a`–`0x727445` is an inline Hamilton product in raw x87 rather
than the vector library. Matching its four stored components term by term against
`q2 ⊗ q1` gives an exact match for x (`0x727417`), y (`0x7273ed`) and w (`0x727445`);
the z component (`0x7273c3`) multiplies q1.y by q1.w where the symbolic product has
q2.z·q1.w, and both are identically zero for every input this code can reach, since
q1's axis is the global Z so q1.y = 0, and q2's axis is the spun X, which lies in the
XY plane, so q2.z = 0. So node`+0x34` becomes **q2 ⊗ q1** — the tilt about the spun
axis first, the azimuth second — stored through `0x70a320` at `0x727448` and copied
into node`+0x34…+0x40` at `0x727457`–`0x727467`.

`0x72746a` then re-tests `IsBind`, and `jne 0x727552` **skips the composition
entirely when bound**. The unbound fall-through (`0x727478`–`0x72752d`) is a single
Hamilton product, **Q ⊗ cone**, against emitter`+0xa0…+0xac` — the composed
orientation from `ParIniDir` and emitter`+0x74` — which matches term for term in all
four components and is stored through `0x70a320` at `0x727530`. The final chain
(`0x727552`–`0x7275f0`) rotates the global **Y** axis `0x9c0348` by node`+0x34` and
writes it to **node`+0x44`**, the direction, at `0x727603`–`0x72760c`.

Composed, and checked numerically to 3e-16 over `Angle` ∈ {0°, 30°, 60°, 90°, 180°}
and θ ∈ {0°, 37°, 140°, 275°}, rotating the global Y by `q2 ⊗ q1` is exactly

    (−sin Angle · sin θ,  sin Angle · cos θ,  cos Angle)

— a cone about the element's local **Z** axis whose half-angle is `Angle` itself,
with the azimuth uniform over the turn. Unlike the shared direction slot's cone,
whose polar angle is drawn uniformly inside `[0, Angle]`, every one of these
particles sits at polar angle `Angle` exactly: the surface of the cone, not its
volume, which is what `IsSurface` means for id 120. The unbound case rotates that
direction by Q as well.

The position follows at `0x72760f`, branching on `IsBind` again. Bound
(`0x727619`): `Vec3::new` of the direction's three components each multiplied by the
float at `0x8ee224`, copied into node`+0x10`. Unbound (`0x72765a`): the direction
scaled by the same constant through `0x401ce0`, added to emitter`+0x68` by
`0x4071f0` at `0x72767a`, copied into node`+0x10`, and — when `IsDrag` at
emitter`+0x86` is set (`0x72768b`) — the emitter position copied into node`+0x28` at
`0x72769d`.

`0x8ee224` is **0.001** (`0x3a83126f`, with π/2 at `0x8ee228` beside it), and the
volume path multiplies by the same constant twice (`0x7276bc`, `0x7276d6`), so it is
the engine's unit conversion on every node position rather than a surface radius.
The consequence is the finding: id 120's surface particles sit **one file unit from
the emitter** on a cone of half-angle `Angle` about its local Z, and the block never
reads `AreaSize` at emitter`+0xb4` or the basis at `+0xc0…+0xe0` at all. Both
products and both `IsBind` re-tests are read now, and `spawn` reproduces the block
for these 86 elements: `surface_cone` for the chain, `to_emitter_local` for the
frame, and the same vector for the position and the direction.

**Id 121's surface sampler `0x727800` is a box face, chosen by area.** It is scalar
x87 from end to end and calls none of the vector helpers. `0x72780d`–`0x727843`
reads `AreaSize` at emitter`+0xb4…+0xbc`, forms the three face products `x·y`,
`y·z` and `x·z`, stores their sum and the ratio `yz/sum`, and one `rand()` draw
(`0x727847`, scaled by the 1/32767 at `0x8e6f10`) then chooses: below `xy/sum`
falls through to `0x72786a`, below `yz/sum` branches to `0x7278e1` (`0x7278d6`–
`0x7278df`), and anything else to `0x727934`. Each branch writes a sign —
`0xbf800000`, replaced by `0x3f800000` when a second draw is not below the 0.5 at
`0x8d5894` (`0x727884`, `0x7278f9`, `0x72794c`) — into the slot the tail reads as
that face's own coordinate, and two `2/32767 − 1` draws (`0x8e6efc`, `0x8d5c28`)
into the other two: `0x7278af`/`0x7278c9` for the Z faces, `0x727924` for the X
faces, `0x727977` for the Y faces. Four draws per particle on every path.

The common tail `0x727991`–`0x727a41` multiplies the three coefficients by the
basis rows at emitter`+0xc0`, `+0xcc` and `+0xd8` and writes node`+0x10…+0x18`, so
the position is a point on the box those nine floats span. No direction is written;
the birth flow calls the direction slot afterwards. Unbound particles then get the
emitter position added at `0x727a6c`, node`+0x34` filled from emitter`+0xa0` at
`0x727a90`, and the drag anchor at node`+0x28` when `IsDrag` (`0x727a5a`). Bound
particles skip all three (`0x727a4e`).

The face test is not the area test. Because `0x7278d6` compares against `yz/sum`
alone rather than the cumulative `(xy+yz)/sum`, the X faces need a draw that is at
once `>= xy/sum` and `< yz/sum`, which happens only when `AreaSize.z >
AreaSize.x`; a cube never emits from its X faces at all. `box_surface_sample`
reproduces that, and a zero extent sends both the client (all three ratios 0/0) and
the sampler (guarded) to the Y-face branch against a zero basis, which is the
origin — 512 corpus elements collapse that way.

### The direction slot `0x723440`, shared by all seven classes

Vtable slot [1] is the same routine in every emitter vtable, and it is what fills
the emission direction at node+0x44 for the classes that call it. It is a thiscall
with one argument — the destination pointer — and `ret $0x4` at `0x7237db`. Two
halves: a base direction, then an optional cone.

**The base direction.** `0x723447` tests `IsBind` at emitter+0x85. Bound: `eax` is
set to `0x9c0338`, the global Z axis, at `0x723451` and copied verbatim into the
destination at `0x723503`–`0x723517` — no rotation at all. Unbound: `0x72345b`
through `0x7234ff` reads the quaternion's vector part at emitter+0x74/0x78/0x7c and
its w at +0x80 and evaluates the same inline Rodrigues rotation the basis builder
uses (`0x4093d0` at `0x72347f` and `0x7234a0`, `0x401cc0` at `0x72348c` and
`0x7234ad`, `0x633700` with the doubled w from `0x7234b8` and with `2.0` at
`0x7234c7`, `0x4071f0` at `0x7234e9` and `0x7234f7`):

```
bound:    node+0x44 = Z
unbound:  node+0x44 = rotate(Z, q),   q = quat(emitter+0x74) = Q ⊗ keypoint.Direction
```

**The cone gate.** `0x72351a` loads `Angle` from emitter+0x10 and compares it with
the 0.0 at `0x8d58a0`; `test $0x40,%ah` after `fnstsw` tests C3, the equality flag,
and `jne 0x7237d6` exits through the epilogue with the base direction already
stored. The gate is therefore **`Angle != 0`, not `Angle > 0`** — the corpus's 36
negative angles do get a cone, mirrored through the emitter. The constant itself is
read out of `.rdata` now: `0x8d58a0` holds the four bytes `00000000`, so it is
`+0.0f` exactly and the comparison really is against zero, with 0.01 at `0x8d5898`,
1000.0 at `0x8d589c` and 800.0 at `0x8d58a4` either side of it.

**The cone.** Two `rand()` draws, through the IAT slot `0x8d5478` loaded once into
`%ebx` at `0x72352f`:

- `0x723541`–`0x723550`: `theta1 = (float)rand() * Angle * (1/32767)`, the polar
  angle, with the constant at `0x8e6f10`. Uniform in `[0, Angle]` and never clamped.
- `0x723626`–`0x723631`: `theta2 = (float)rand() * (2*pi/32767)`, the azimuth,
  constant `0x8e6f14`, so a full turn.

The cone's own axis is `0x9c0358` (global X) when bound (`0x723556`) and
`rotate(X, q)` when unbound (`0x723560`–`0x723604`, the same Rodrigues spelling).
X is perpendicular to Z and the rotation preserves that, so the axis is
perpendicular to the base direction in both cases. The construction is then:

```
q_az   = axis_angle(node+0x44, theta2)   ; 0x7b6fd0 at 0x72363b, quaternion out at [esp+0x70]
axis'  = rotate(axis, q_az)              ; 0x723661-0x7236e9, Rodrigues again
q_tilt = axis_angle(axis', theta1)       ; 0x7b6fd0 at 0x723718
node+0x44 = rotate(node+0x44, q_tilt)    ; 0x723741-0x7237c1, copied in at 0x7237d0
```

Spin the perpendicular axis about the base direction by the azimuth, then tilt the
base direction away from itself by the polar angle about that spun axis. Both
`0x7b6fd0` calls are cdecl and the caller cleans them together with the rotation
chain's pushes — `add $0x18,%esp` at `0x723675` and `0x723732` — which is the same
leftover-argument convention the basis rows use. Slot [2] beside it, `0x7237e0`, is
two instructions, `flds 0x2c(%ecx); ret`: it returns emitter+0x2c, the TTL.

**`ParIniDir` is not part of the direction.** The slot reads only `IsBind` and the
orientation quaternion at emitter+0x74; it never touches +0xa0. `ParIniDir`'s
Euler-to-quaternion result is composed into emitter+0xa0…+0xac (parse at
`0x7243b8`, runtime at `0x71b450`) and the birth routines copy those four floats
into node+0x34 — the particle's initial *orientation*, a different field from the
direction at node+0x44.

### The emitter object

The vtables are consecutive in `.rdata`, twelve slots each, so all seven can be
read off in one dump (`objdump -s -j .rdata`, `0x8edf24`–`0x8ee0ec`). The base
row is what the shared constructor writes before the per-class tail overwrites
its own slots.

| Class | Vtable | [0] | [1] | [2] | [3] | [4] | [5] | [6] | [7] | [8] birth | [9] basis | [10] | [11] |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| base | `0x8edf24` | `723410` | `723440` | `7237e0` | `7237f0` | `723a80` | `723a20` | `723a50` | `892f56` | `724ab0` | `56fbb0` | `724b20` | `7248d0` |
| 120 | `0x8ee0bc` | `726540` | " | " | " | " | " | " | `726530` | `727230` | **`56fbb0`** | `724b20` | `7248d0` |
| 121 | `0x8ee078` | `725e20` | " | " | " | " | " | " | `726530` | `727760` | `729670` | `724b20` | `729b00` |
| 122 | `0x8ee034` | `7264b0` | " | " | " | " | " | " | `7288e0` | `7288f0` | **`56fbb0`** | `724b20` | `7290b0` |
| 123 | `0x8edff0` | `725e20` | " | " | " | " | " | " | `726530` | `727e40` | `729670` | `724b20` | `727c20` |
| 124 | `0x8edfac` | `725e20` | " | " | " | " | " | " | `726530` | `729b40` | `729670` | `724b20` | `729b00` |
| 125 | `0x8edf68` | `725c00` | " | " | " | " | " | " | `7256b0` | `726570` | `727220` | `724b20` | `726ff0` |

Six of the twelve slots are identical in all seven tables — `[1]` the direction,
`[2]` the TTL, `[3]` the colour, `[4]` the phase, `[5]` the scale, `[6]` the
rotation and `[10]` `0x724b20`. Slot `[9]` is *not*: ids 121, 123 and 124 get the
basis builder `0x729670` that fills `+0xc0…+0xe0` from `AreaSize`, while 120 and
122 get `0x56fbb0`, an engine routine outside the gfx region, and 125 gets its own
`0x727220`. That is why id 120's surface block reads no basis at all — its class
never builds one.

The shared constructor is `0x723250`. It zeroes `+0x1c/+0x20`, sets `+0x24` to
`-1.0f` (the constant at `0x8d5c2c`), zeroes the position at `+0x68…+0x70`, loads
the orientation quaternion at `+0x74…+0x80` from four globals
(`0x9c0318`, `0x9c031c`, `0x9c0320`, `0x9c0324`), zeroes the three Euler angles at
`+0x94/+0x98/+0x9c` and `+0xb0`, writes the base vtable at `0x7232b2`, sets
`+0x04/+0x30/+0x34` to `-1` and calls `0x789ad0`. The four orientation globals sit
on the same `.data` page as the three unit axes the samplers use
(`0x9c0338`, `0x9c0348`, `0x9c0358`) and are initialized at run time, so their
values are not in the file.

`+0xa0…+0xac` is not the colour the earlier offset map called it. Two sites write
it, and both compute the same thing: the emitter's three Euler angles converted to
a quaternion and Hamilton-multiplied by the orientation quaternion at
`+0x74…+0x80`. At parse time that is `0x7242ee` (`sscanf` through `%ebx` fills
`+0x94/+0x98/+0x9c`), `0x724313` (`0x7b6f10`, Euler→quaternion), the products
against `+0x80/+0x74/+0x78/+0x7c` from `0x72431c`, and the destination
`lea 0xa0(%edi),%eax` at `0x7243b8`. At run time `0x71b450` repeats it exactly —
`flds +0x9c/+0x98/+0x94`, `call 0x7b6f10` at `0x71b479`, the same four products,
then `add $0xa0,%esi` at `0x71b492` and four dword writes at
`0x71b524/0x71b52c/0x71b537/0x71b53a`, `ret` at `0x71b541`. The birth routines copy
those four floats into node`+0x34` (`0x728241`–`0x72825d` for id 123), which is
what that node field carries.

The orientation quaternion itself does have a writer, at `0x716025`, inside the
element's spawn loop (`0x715f00`–`0x716176`, with a second copy of the tail at
`0x7160ca`–`0x716146`). Per iteration the loop takes a node from the free list at
element`+0x198` (`call 0x6e00b0` at `0x715f14`), links it (`call 0x4287c0` at
`0x715f22`), interpolates a position as `t·(*ebx) + (1−t)·(element+0x210,
+0x214, +0x218)` with `t = index × [esp+0x34]` (two `Vec3::new` calls at
`0x715f9d` and `0x715fca`), builds a quaternion with four arguments at
`call 0x7b7010` (`0x716003`, one of them `&element+0x21c`), then takes
`ebp = element+0x140` and writes **emitter`+0x68…+0x70`** from the interpolated
vector (`0x716019`, via `0x401cc0`) and **emitter`+0x74…+0x80`** as four dwords
copied from `[esp+0xb8]` (`0x716025`–`0x71603b`), followed immediately by
`call 0x71b450` at `0x716040` to recompose `+0xa0`. So the emitter's position and
orientation both move as the element spawns, and the unbound emission basis is
rotated by a real orientation rather than the constructor's initial one.

What that loop reads is decoded now too. `element+0x210…+0x218` and
`element+0x21c…+0x228` are the element's own cache of the emitter's position and
orientation, written by the element update at `0x71a537`–`0x71a54f` (position, from
emitter`+0x68`) and `0x71a56c`–`0x71a57b` (quaternion, from emitter`+0x74`), so the
loop interpolates from where the emitter was on the previous frame to where it is
now. `[esp+0xb8]` is that interpolation's output, and `0x7b7010` is a quaternion
**slerp** rather than a plain blend: `|dot| > 0.75` (`0x8f0aec`) takes the linear
path `(1−t)a + sign·t·b`, otherwise the coefficients are `sin((1−t)θ)/sinθ` and
`sign·sin(tθ)/sinθ` with `θ = acos(dot)`, and `b` is negated when `dot < 0` so the
interpolation always goes the short way round (stores at `0x7b7100`). The entry
guard at `0x715f00` is only `element+0x198 != 0`, so the loop runs whenever the
element has a spawn queue at all.

**The basis scalar is keypoint `Scale` × root `PlaySpeed`.** Slot [9]'s only two
call sites are the two `IsBind` arms of the element update `0x71a1e0` (`0x71a351`
reads emitter`+0x85`, `0x71a359` skips the call when it is zero), and both compute
the same product, `[esp+0x48] × [esp+0x24]`, at `0x71a35b` (bound) and `0x71a501`
(unbound). Slot `0x24` is filled at `0x71a225` from `[element+0x10]+0x1ec` — the
root's `PlaySpeed`. Slot `0x48` is one float of the interpolated keypoint record the
routine builds at `[esp+0x38]` through `0x6fe0b0`, and the surrounding code pins
that record's layout down exactly: a `Vec3` at `+0x38`, a packed colour at `+0x44`
whose top byte is scaled by `1/255` (`0x8eadc0`) into emitter`+0x8c` at `0x71a337`
(the live alpha the parity assessment saw `0x7237f0` folding in), the float at
`+0x48`, and four floats at `+0x4c…+0x58`. That is the `.gfx` keypoint's
`Position`/`Color`/`Scale`/`Direction`, with `gfx_file::KeyPoint` as its
byte-for-byte counterpart; `+0x48` is separately pushed three times as a uniform
vector into `0x787de0` at `0x71a346` to fill the element's own scale at `+0x1b8`.
Both scalar reads happen **before** the affector update `0x716240` at `0x71a37c`, so
a controller's scale override never reaches the emission volume.

**The unbound basis, end to end.** `0x729763` computes `V = scalar × AreaSize` —
reading its single argument three times, at `0x729784`, `0x72979f` and `0x7297c8`,
where the `push`/`fstps (%esp)` idiom has shifted the frame so that `0x74(%esp)`,
`0x7c(%esp)` and `0x80(%esp)` are all `[entry+4]` — builds the quaternion's vector
part from emitter`+0x74/0x78/0x7c` at `0x7297ad`, and writes row *i* as
`rotate(axis_i, q) × (V_i × 0.5)`. Rows 0 and 1 evaluate the rotation inline as
`G + 2w(Q × G) + 2(Q × (Q × G))` (`0x4093d0` at `0x7297d4` and `0x7297fe`,
`0x633700` with the doubled w from `0x7297f4` and with `2.0`, `0x4071f0` twice with
one argument deliberately left on the stack); row 2 reaches the same result through
`0x634420` (`0x729972`) and `0x401ce0` (`0x729980`). The rows are stored at
`0x72987c` (`+0xc0`), `0x72994d` (`+0xcc`) and `0x729988` (`+0xd8`), against
`0x9c0358`, `0x9c0348` and `0x9c0338` with `V.x`, `V.y` and `V.z`:

```
bound   (+0x85 != 0):  row_i = G_{2-i}            × (AreaSize_i × 0.5)
unbound (+0x85 == 0):  row_i = rotate(G_{2-i}, q) × (scalar × AreaSize_i × 0.5)
                       q     = quat(emitter+0x74) = Q ⊗ keypoint.Direction
```

The bound branch at `0x729684` never touches the scalar argument, which is why it
changes unbound emitters only.

**Where `Q` comes from.** `0x6fa810(element)` reads `[element+0x10]` and returns
**`&root+0x1b4`** when root`+0x1f4` and element`+0x131` are set and either
element`+0x132` or root`+0x284` is, otherwise **`&root+0x1a4`** — the effect's own
placement orientation. The unbound arm multiplies it by the keypoint record's
direction at `0x71a406`–`0x71a493` (`w'` at `0x71a406`, `x'` at `0x71a42b`, `y'` at
`0x71a450`, `z'` at `0x71a479`, staged in slots `0x28/0x2c/0x30/0x34`), stores the
product at emitter`+0x74…+0x80` (`0x71a4dc`–`0x71a4f1`), fills the world position at
`+0x68/0x6c/0x70` from slots `0x10/0x14/0x18` (`0x71a4b5`, the position `0x7872d0`
returns when element`+0x1f8` is set, otherwise `0x71a3f1`), and calls `0x71b450` at
`0x71a4f4` to recompose `+0xa0`. The product's order was checked term by term
against both Hamilton expansions: the scalar part matches either, but the vector
parts match only **`Q ⊗ keypoint`**, so the keypoint direction is applied first and
the root orientation second. Reading that needed care with the x87 mnemonics — GAS
prints `fsubrp %st,%st(1)` for `st(1) − st(0)`, and taking it with Intel's operand
order reverses every difference term.

## Runtime player

The root clock advances in milliseconds and is scaled by `PlaySpeed`; controllers integrate in seconds of scaled time. An element's clock is its keypoint spans plus `RepeatDelay`, repeated `RepeatCount` times (1 = once, −1 = open-ended, N = N passes, and the corpus's 10,000/2147483647 values are "effectively forever"); a phase no keypoint covers draws nothing but does not end the element, and the root reports itself finished only when *every* element is over and none loops. `RotFromView` decides where a sprite's basis comes from — 598 retail sprites take it from the camera, 8,271 keep the authored keypoint direction — and the root's `FaceToViewer` forces the camera basis for all of them. Flipbook cells walk `floor(clock/TexInterval) % clamp(TexAnimMaxTime/TexInterval, 1, rows*cols)` with v growing downwards, and `TexSpeed` scrolls U and V continuously. `InterpolateMode` is 0 in every corpus keypoint, so linear blending between keypoints is implemented but never exercised by retail data.

Children (element 200) resolve their `GfxPath` at load time, exactly like the client's `A3DGFXMan::LoadGFXFromFile`, with a depth cap of 6 and a path stack to stop cycles. Of the corpus's 1,530 child references, 1,448 resolve inside `gfx.pck` after `\`→`/`, lowercasing and the `gfx/` prefix; the 82 that do not are 31 distinct paths that genuinely do not ship (a doubled directory `人物\技能\巫师\技能\…`, a stray space in `4品法宝_冷凝术_ 子效果.gfx`). Those are authoring mistakes, not a resolver bug, so an unresolvable child is a named warning through `Player::warnings()` and the element degrades to a clock that emits nothing, instead of the whole root being dropped. 49 roots (0.9%) are affected.

## Verification

`gfx_file` carries 23 tests over verbatim retail records and rejection paths; `gfx_player` carries 15 more, including the controller integrals, the colour-remainder carry, quota and TTL unlinking, flipbook walking, basis selection, distance fade and the alpha cutoff, repeat/delay gating, and the child warning, cycle and nesting cases. One test is a cross-module equivalence check: for a root both readers accept, the player's frame zero must match `gfx_decal::Root::quads()` corner for corner, UV for UV and channel for channel. The same check was run over the whole corpus — 13 retail roots, 19 quads, 0 mismatches — which is the strongest available evidence for the sprite path, because `gfx_decal` is the reader with native decal calls behind it.

The full-corpus sweep parses every member, builds a player for each root, advances two players 90 frames each under `catch_unwind` and compares them for determinism. Final run: 5,458 files walked, 5,456 parsed, 5,456 played, 662 child roots loaded, 25,455,994 quads emitted, 4,095 quads in a single frame, 4,257 live particles, 2 parse failures (an empty container and a version-4 root that predates the effect format), 0 build failures, 0 advance failures, 0 non-deterministic roots, 0 panics, 168 child warnings, 0 cycles. One member is named exactly `.gfx` and is skipped by extension-based walkers; it is a valid version-33 root with 15 elements.

Four real bugs were found by that verification rather than by inspection, and all four are now pinned by tests:

- The corner construction mixed scaled and unscaled extents (`right = Width*2 − left` after `left` had already been scaled) and put the origin split on the top edge instead of the bottom. Invisible at `OrgPt: 0.5`, wrong for the 704 off-centre lines.
- The controller displacement was transcribed as `v*dt + 0.5*(2*v*dt + Acc*dt²)`, which double-counts the velocity term and would have made every controller-driven motion twice as fast. Re-reading `0x6eaf60` and `0x6eb230` settled it: the expression is the trapezoid `0.5*(2*v + Acc*dt)*dt`, which reduces to `v*dt` at zero acceleration and is what makes `Vel: 1.745329` mean 100°/s.
- `ColorDelta`'s four integers were applied as A, R, G, B. The client's unpack at `0x6ebfab` and repack at `0x6ec03f` are R, G, B, A, so every authored alpha fade was fading blue instead, and one shared remainder was used where the client keeps one per channel and clears it on saturation.
- Alpha was folded continuously (`a/255 * scale`) where the verified decal folds in 0..255 units and truncates. At `DefaultAlpha: 0.5` that is 127/255, not 127.5/255, and the two readers disagreed on every half-transparent effect.

Seven validations were also removed after the corpus disproved them: negative `Width`/`Height` (530 occurrences), out-of-range `OrgPt` (704), inverted `ScaleMin`/`ScaleMax` (817), non-positive keypoint `Scale` (18, including uninitialized editor garbage such as `Scale: -94883208` and `Position: 1.8e28` in `gfx/策划联入/人物技能/击中/巫师_冷凝术_击中.gfx`), non-positive non-keypoint `TimeSpan` (440) and `DefaultAlpha > 1` (2). They are now clamped or sampled as authored instead of rejecting a shipping file. What is still rejected: non-finite values, versions outside 24…79, an empty container, a malformed `ScaleChage`, a grid colour count that disagrees with `wNumber*hNumber`, unknown keys, keys out of positional order, counts above their ceilings, `FadeStart > FadeEnd` (0 occurrences in 5,457 files), negative `PlaySpeed` and non-positive `DedaultScale`.

## Interpreted, not verified

The emitter shape classes were never located in the binary, so box and sphere spawns distribute uniformly inside the authored `AreaSize`/`Radius`; that is the one native behaviour deliberately not copied. **Superseded the same day:** the shape classes were subsequently located (factory `0x723ab0`, birth `0x727e40`, shape `0x728270`, matrix build `0x729670`), `AreaSize` turned out to be an ellipsoid of semi-axes `AreaSize/2` rather than a box, and the cone and colour draws were corrected with it — see [the parity assessment](Gfx-effects-parity-assessment-2026-11-05.md). The fractal-noise generator behind controllers 107, 109 and 112 is not decoded — the player implements value noise over `BufLen`, `Amplitude`, `WaveLen`, `Persistence` and `OctaveNum` with a documented hash, and 241 root controllers in the corpus are all type 112, so this is the most-used unverified path. Ground matching of spawned particles (`0x41a430`) is skipped. Controller 105's native remainder slots sit at ctrl+0x24…+0x30 on the controller object, which the earlier reading confused with the particle temp's rotation slot at temp+0x24; the interference that confusion implied does not exist and is no longer claimed. `gfx_light` still passes raw milliseconds into `ColorTrack::new`, whose `sample()` takes seconds — latent and harmless, but unresolved. Element 180 and any ID the client never instantiates are counted as unrendered rather than guessed at.

## Integration

`src/modules/walker/gfx.rs` keeps every existing event gate (BindParent 1, Once 0, StartTime 0, TimeSpan −1, no `GfxOuterPath`/`GfxRelToECM`, model alpha) and now decodes through `gfx_file::decode`, offers the text to `gfx_decal::Root::parse` first, and falls back to `gfx_file::parse` plus `Player::new` with a `load_child` closure that reads the nested member from the same archive. The event's `GfxSpeed` finally has something to act on: dynamic roots are advanced by `dt * speed`, which is the slot the client scales its millisecond delta by. `src/modules/walker/mod.rs` gained a `gfx_effect_gpu::Draw` next to the static decal draw, sharing the reverse-Z `Depth32Float`/`Greater` attachment, and draws it immediately after the static quads. `render` carries no delta, so the wall clock between frames supplies one, clamped to 0…100 ms, and the camera basis is recovered from the inverse of the frame's view-projection. The whole-project build, the GPU checks and the formatting gate can only be confirmed by CI: wgpu and egui are not reachable from the sandbox, so `gfx_effect_gpu.rs` and the two walker files were never compiled locally, while `gfx_file.rs`, `gfx_player.rs`, `gfx_color_track.rs` and `gfx_decal.rs` were compiled and tested against a vendored glam with the real `encoding_rs` replaced by a byte-preserving stub.

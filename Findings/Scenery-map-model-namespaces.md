# Scenery BMD lookup must retain the map namespace

## Verified root cause

The EC-BSD stores scenery model paths relative to a numbered subdirectory, for
example `1/litmodel_1318.bmd`. The retail `litmodels.pck` does not put these
members under one universal `litmodels/world/` root. It partitions them by map:

- A01: `litmodels/a01/1/litmodel_1318.bmd`
- A11 also has a distinct `litmodels/a11/1/litmodel_1318.bmd`
- A02: `litmodels/a02/1/litmodel_0.bmd` through `litmodel_31.bmd`

The previous resolver normalized every relative reference to
`litmodels/world/<relative-path>`, then used a world-relative suffix search.
That discarded the source map ID. It explains both reported error shapes: A01
models such as 1318 were reported missing or ambiguous between A01 and A11,
while `litmodel_0` on other maps had eight suffix matches. The models were in the
archive; the renderer was searching the wrong namespace. With no BMD loaded,
texture lookup never ran, so zero geometry and zero decoded texture residency
were downstream symptoms, not separate causes.

## Corpus check

- The 397,425,471-byte PCK (MD5 `359be12bc2aab785210ab82506a67aaf`) is the
  same archive previously surveyed: 28,858 members, all BMDs; the existing
  Phase 3 census parsed all 28,858 with no read or BMD parse errors.
- The supplied A01 EC-BSD (MD5 `fd8e6a19f114903f0d230c9e4f555bc3`) contains
  328 unique relative model references. Every one (328/328) has an exact
  `litmodels/a01/<reference>` member in that PCK. In particular, the first
  reported model exists as `litmodels/a01/1/litmodel_1318.bmd`.
- The PCK contains exactly 32 A02-prefixed scenery BMD members, numbered
  `litmodel_0` through `litmodel_31`, matching the reported 32-placement case.

This establishes a verified path-lookup bug against the staged retail archive
and map data. It is not a claim that every map's source data or rendered output
has now been runtime-verified.

## Fix in code

`Scene::load_ornaments` now retains the map ID from the EC-BSD filename on each
ornament. `Objects` first resolves `litmodels/<map-id>/<relative-path>`; when a
map-specific namespace exists, a missing member is reported within that
namespace rather than being silently borrowed from another map. The main-world
legacy fallback remains available for archives whose layout is not
map-partitioned. Regression tests cover A01/A02 disambiguation and refusing a
cross-map fallback.

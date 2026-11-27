# Architect

Architect is an optional world-authoring workspace registered through the normal module package system.

## Entering and leaving

Open the main menu and choose ARCHITECT | WORLD EDITOR. The original HUD and viewer panels are hidden while the existing GPU world renderer remains the viewport. Use the red Turn off Architect control in the top toolbar to return to the normal viewer.

## Workspace

- World (left by default): choose an empty project, any readable client map, or a saved Architect project. Client map changes use the viewer's existing world-handoff path and reopen Architect in the destination map.
- Client assets (right by default): browse the client folder tree lazily, expand PCK archive indexes on demand, and right-click folders/files/archive entries to pin them to the quick bar.
- Scene objects: inspect loaded models, trees, scenery and effects; multi-select and duplicate eligible ECM models into the editable project layer.
- Transform: edit custom placements' position, Euler rotation (degree-based controls) and scale. Supported ECM placements update in the live viewport through the same native model, material, animation and GPU queue used by map models.
- Terrain tools: apply raise/lower/flatten/smooth brushes at the camera position with a configurable radius and strength. Height and normal overrides are shared with navigation and the renderer; the near terrain blocks and distant LOD rebuild when the edit revision changes. Brush strokes are stored in the project and support undo/redo. Paint is shown as an unavailable mode until terrain material painting is implemented.
- Quick bar (bottom): select pinned assets quickly.

Panels are movable and can be repositioned to the left, right, bottom or floating placement. Directory and PCK indexes are cached until Refresh files is used.

## Project storage

Projects are saved as portable UTF-8 .pwa files under the user's application data directory:

- Windows: %LOCALAPPDATA%/PerfectWorldExplorer/Architect/Worlds
- Linux: $XDG_DATA_HOME/PerfectWorldExplorer/Architect/Worlds, or ~/.local/share/PerfectWorldExplorer/Architect/Worlds

The project format stores a source map ID, asset references, object transforms and ordered terrain-brush operations. It is versioned and validates numeric values and file size on load. Loading a project replays its strokes over the immutable source terrain; undo/redo rebuilds that overlay from the stroke history.

## Current implementation boundary

The live project layer currently instantiates ECM models referenced from models.pck. Other PCK formats and loose files remain browseable and pinnable but are not guessed into unsupported renderers. Terrain sculpting is a sparse, non-destructive in-memory overlay: it updates heights and recalculated normals for the shared navigation/renderer data, and forces both resident near terrain and distant LOD to rebuild. It never modifies the client's original T2BK/ECWLD files. Paint/material editing and native map export remain unavailable until validated material, ECWLD, EC-BSD and terrain writers exist.

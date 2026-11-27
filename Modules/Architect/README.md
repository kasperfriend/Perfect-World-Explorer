# Architect

Architect is an optional world-authoring workspace registered through the normal module package system.

## Entering and leaving

Open the main menu and choose ARCHITECT | WORLD EDITOR. The original HUD and viewer panels are hidden while the existing GPU world renderer remains the viewport. Use the red Turn off Architect control in the top toolbar to return to the normal viewer.

## Workspace

- World (left by default): choose an empty project, any readable client map, or a saved Architect project. Client map changes use the viewer's existing world-handoff path and reopen Architect in the destination map.
- Client assets (right by default): browse the client folder tree lazily, expand PCK archive indexes on demand, and right-click folders/files/archive entries to pin them to the quick bar.
- Scene objects: inspect and viewport-pick loaded models, trees, scenery and effects; duplicate eligible ECM models into the editable project layer. Non-ECM source objects remain read-only.
- Transform and hierarchy: custom ECM placements appear in collapsible groups in the Transform panel. Each placement has an editable name and group, plus Visible and Locked flags. Groups have Select group, Hide, Show, Lock and Unlock actions. Hidden placements are omitted from the live renderer and viewport picking; locked placements remain visible/selectable but cannot be moved. The hierarchy filter searches placement names and groups. Projects are now written as PWA version 2; version 1 projects migrate on load with sensible default names, the Default group, visible=true and locked=false. Click staged custom ECM placements in the viewport to select them; the selected placement receives a three-axis gizmo with X/Y/Z translation handles and ground-plane free translation. Position, Euler rotation (degree-based controls) and scale remain editable numerically. Use Multi-select to build a selection from viewport clicks or the placement list; the Transform panel also has Select all, Clear selection, Duplicate selected and Delete selected actions. With multiple placements selected, switch back to Select and drag on the terrain to translate the unlocked selected group together while preserving relative offsets. Group moves are committed as one object-history operation. Object edits have bounded undo/redo history. Supported ECM placements update in the live viewport through the same native model, material, animation and GPU queue used by map models.
- Placement: select an ECM entry and choose Place, then click terrain in the viewport to instantiate it with the selected snapping mode.
- Terrain tools: choose Sculpt in the viewport toolbar, then click-drag directly over terrain. A projected, terrain-conforming brush ring shows the active radius; raise/lower/flatten/smooth dabs follow the cursor and are saved as ordered brush operations. The camera-position brush remains available as a fallback. Height and normal overrides are shared with navigation and the renderer; the near terrain blocks and distant LOD rebuild when the edit revision changes. Brush strokes are stored in the project and support undo/redo. Paint is shown as an unavailable mode until terrain material painting is implemented.
- Quick bar (bottom): select pinned assets quickly.

The Multi-select cursor mode is intended for editing the custom project layer. Source-scene multi-selection remains a separate read-only inspection/duplication workflow. Group names are currently flat labels (not nested parent/child folders); visibility and lock state apply per placement.

Panels are movable and can be repositioned to the left, right, bottom or floating placement. Directory and PCK indexes are cached until Refresh files is used.

## Project storage

Projects are saved as portable UTF-8 .pwa files under the user's application data directory:

- Windows: %LOCALAPPDATA%/PerfectWorldExplorer/Architect/Worlds
- Linux: $XDG_DATA_HOME/PerfectWorldExplorer/Architect/Worlds, or ~/.local/share/PerfectWorldExplorer/Architect/Worlds

The project format stores a source map ID, asset references, object transforms and ordered terrain-brush operations. Saves validate object/brush values and stage a temporary file while preserving the previous project as a rollback backup during replacement. Placements are not rendered when the active map does not match the project's source map. It is versioned and validates numeric values and file size on load. Loading a project replays its strokes over the immutable source terrain; undo/redo rebuilds that overlay from the stroke history.

## Current implementation boundary

The live project layer currently instantiates ECM models referenced from models.pck. Other PCK formats and loose files remain browseable and pinnable but are not guessed into unsupported renderers. Terrain sculpting is a sparse, non-destructive in-memory overlay: it updates heights and recalculated normals for the shared navigation/renderer data, and forces both resident near terrain and distant LOD to rebuild. It never modifies the client's original T2BK/ECWLD files. Paint/material editing and native map export remain unavailable until validated material, ECWLD, EC-BSD and terrain writers exist.

//! Movable & pinnable tab docking system for Architect.
//!
//! Every editor tab (`World Selector`, `Scene Outliner`, `Client Explorer`,
//! `Inspector`, `Quick Bar`) can be pinned to the Left, Right, or Bottom dock
//! or popped out into a Floating window, and users can switch active tabs or
//! move tabs between docks with a single click.

/// Where a tab is currently docked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockSide {
    Left,
    Right,
    Bottom,
    Floating,
}

impl DockSide {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Bottom, Self::Floating];

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Bottom => "Bottom",
            Self::Floating => "Float",
        }
    }
}

/// Every dockable panel tab in Architect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorTab {
    WorldSelector,
    SceneOutliner,
    ClientExplorer,
    Inspector,
    QuickBar,
}

impl EditorTab {
    pub const ALL: [Self; 5] = [
        Self::WorldSelector,
        Self::SceneOutliner,
        Self::ClientExplorer,
        Self::Inspector,
        Self::QuickBar,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::WorldSelector => "Worlds",
            Self::SceneOutliner => "Outliner",
            Self::ClientExplorer => "Client Explorer",
            Self::Inspector => "Inspector",
            Self::QuickBar => "Quick Bar",
        }
    }

    pub fn default_dock(self) -> DockSide {
        match self {
            Self::WorldSelector => DockSide::Left,
            Self::SceneOutliner => DockSide::Left,
            Self::ClientExplorer => DockSide::Right,
            Self::Inspector => DockSide::Right,
            Self::QuickBar => DockSide::Bottom,
        }
    }
}

/// Persistent docking state for all Architect tabs.
#[derive(Clone, Debug)]
pub struct DockLayout {
    pub left_tabs: Vec<EditorTab>,
    pub right_tabs: Vec<EditorTab>,
    pub bottom_tabs: Vec<EditorTab>,
    pub floating_tabs: Vec<EditorTab>,
    pub active_left: Option<EditorTab>,
    pub active_right: Option<EditorTab>,
    pub active_bottom: Option<EditorTab>,
    pub left_width: f32,
    pub right_width: f32,
    pub bottom_height: f32,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            left_tabs: vec![EditorTab::WorldSelector, EditorTab::SceneOutliner],
            right_tabs: vec![EditorTab::ClientExplorer, EditorTab::Inspector],
            bottom_tabs: vec![EditorTab::QuickBar],
            floating_tabs: Vec::new(),
            active_left: Some(EditorTab::WorldSelector),
            active_right: Some(EditorTab::ClientExplorer),
            active_bottom: Some(EditorTab::QuickBar),
            left_width: 270.0,
            right_width: 310.0,
            bottom_height: 92.0,
        }
    }
}

impl DockLayout {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn side_of(&self, tab: EditorTab) -> DockSide {
        if self.left_tabs.contains(&tab) {
            DockSide::Left
        } else if self.right_tabs.contains(&tab) {
            DockSide::Right
        } else if self.bottom_tabs.contains(&tab) {
            DockSide::Bottom
        } else {
            DockSide::Floating
        }
    }

    pub fn tabs_on(&self, side: DockSide) -> &[EditorTab] {
        match side {
            DockSide::Left => &self.left_tabs,
            DockSide::Right => &self.right_tabs,
            DockSide::Bottom => &self.bottom_tabs,
            DockSide::Floating => &self.floating_tabs,
        }
    }

    pub fn active_on(&self, side: DockSide) -> Option<EditorTab> {
        let tabs = self.tabs_on(side);
        if tabs.is_empty() {
            return None;
        }
        let current = match side {
            DockSide::Left => self.active_left,
            DockSide::Right => self.active_right,
            DockSide::Bottom => self.active_bottom,
            DockSide::Floating => tabs.first().copied(),
        };
        current
            .filter(|t| tabs.contains(t))
            .or_else(|| tabs.first().copied())
    }

    pub fn set_active(&mut self, side: DockSide, tab: EditorTab) {
        match side {
            DockSide::Left => self.active_left = Some(tab),
            DockSide::Right => self.active_right = Some(tab),
            DockSide::Bottom => self.active_bottom = Some(tab),
            DockSide::Floating => {}
        }
    }

    /// Move `tab` to `target` dock side and make it the active tab there.
    pub fn move_tab(&mut self, tab: EditorTab, target: DockSide) {
        self.left_tabs.retain(|&t| t != tab);
        self.right_tabs.retain(|&t| t != tab);
        self.bottom_tabs.retain(|&t| t != tab);
        self.floating_tabs.retain(|&t| t != tab);

        match target {
            DockSide::Left => {
                self.left_tabs.push(tab);
                self.active_left = Some(tab);
            }
            DockSide::Right => {
                self.right_tabs.push(tab);
                self.active_right = Some(tab);
            }
            DockSide::Bottom => {
                self.bottom_tabs.push(tab);
                self.active_bottom = Some(tab);
            }
            DockSide::Floating => {
                self.floating_tabs.push(tab);
            }
        }

        // Fix any active pointer whose tab was moved away.
        if self
            .active_left
            .is_some_and(|t| !self.left_tabs.contains(&t))
        {
            self.active_left = self.left_tabs.first().copied();
        }
        if self
            .active_right
            .is_some_and(|t| !self.right_tabs.contains(&t))
        {
            self.active_right = self.right_tabs.first().copied();
        }
        if self
            .active_bottom
            .is_some_and(|t| !self.bottom_tabs.contains(&t))
        {
            self.active_bottom = self.bottom_tabs.first().copied();
        }
    }
}

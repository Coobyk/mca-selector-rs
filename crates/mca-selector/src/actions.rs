use gpui::actions;

actions!(
    mca_selector,
    [
        QuitAction,
        OpenWorldAction,
        OpenDimensionAction,
        OpenRecentAction,
        SettingsAction,
        RenderSettingsAction,
        ReloadAction,
        ChunkGridAction,
        RegionGridAction,
        CoordinatesAction,
        CoordStyleDynamicAction,
        CoordStyleEdgeAction,
        CoordStyleGridAction,
        StructureIconsAction,
        GotoAction,
        ResetZoomAction,
        SaveScreenshotAction,
        ClearViewCacheAction,
        ClearAllCacheAction,
        ClearSelectionAction,
        InvertSelectionAction,
        InvertRegionsAction,
        CopyChunksAction,
        PasteChunksAction,
        DefragmentAction,
        ExportChunksAction,
        DeleteChunksAction,
        ImportSelectionAction,
        ExportSelectionAction,
        ExportImageAction,
        ClearSelectionCacheAction,
        ImportChunksAction,
        FilterChunksAction,
        ChangeFieldsAction,
        EditNBTAction,
        SwapChunksAction,
        EditOverlaysAction,
        NextOverlayAction,
        NextOverlayTypeAction,
        SumSelectionAction,
        AboutAction,
    ]
);

/// data-carrying action so each recent-world menu item opens its own path.
#[derive(gpui::Action, serde::Deserialize, serde::Serialize, Clone, PartialEq, gpui::private::schemars::JsonSchema)]
#[action(namespace = mca_selector)]
pub struct OpenRecentWorld {
    pub path: String,
}

#[derive(gpui::Action, serde::Deserialize, serde::Serialize, Clone, PartialEq, schemars::JsonSchema)]
#[action(namespace = mca_selector)]
pub struct ToggleStructureIcon {
    pub id: String,
}

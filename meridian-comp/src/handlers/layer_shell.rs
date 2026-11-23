//! wlr-layer-shell: surfaces that are part of the desktop itself (bar, dock,
//! wallpaper, launcher) rather than application windows.
//!
//! A layer surface anchors itself to screen edges and may reserve an
//! "exclusive zone": space that normal windows should stay out of.

use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
            WlrLayerShellState,
        },
    },
};

use crate::state::Meridian;

impl WlrLayerShellHandler for Meridian {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // The client may ask for a specific monitor; otherwise use the first.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            tracing::warn!("layer surface \"{namespace}\" created but there is no monitor");
            return;
        };

        tracing::info!("layer surface \"{namespace}\" on {}", output.name());
        // Each output has a "layer map" that tracks its layer surfaces.
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("could not map layer surface: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|layer| layer.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        // The reserved space is free again.
        self.apply_layout();

        // If it held the keyboard (a closing launcher), hand the keyboard
        // back to the topmost window.
        if self.focused_surface.as_ref() == Some(surface.wl_surface()) {
            let top = self.space.elements().next_back().cloned();
            self.focus_window(top.as_ref());
        }
    }
}

impl Meridian {
    /// Called on every surface commit. Returns true if it was a layer surface.
    pub fn layer_commit(&mut self, surface: &WlSurface) -> bool {
        let Some(output) = self
            .space
            .outputs()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return false;
        };

        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });

        // `arrange` computes positions from anchors, margins and sizes, and
        // reports whether the space left for windows changed.
        let (zone_changed, wants_keyboard) = {
            let mut map = layer_map_for_output(&output);
            let changed = map.arrange();
            let layer = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL);
            if !initial_configure_sent
                && let Some(layer) = &layer
            {
                // Same handshake as for windows: the first configure tells
                // the client its size, then it may draw.
                layer.layer_surface().send_configure();
            }
            // "Exclusive" keyboard interactivity: the surface wants all typing
            // while it is shown. Launchers and lock screens ask for this.
            let wants_keyboard = layer.is_some_and(|layer| {
                layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && matches!(layer.layer(), Layer::Top | Layer::Overlay)
            });
            (changed, wants_keyboard)
        }; // the map's lock is released here, before we touch windows

        if zone_changed {
            self.apply_layout();
        }
        if wants_keyboard
            && initial_configure_sent
            && self.focused_surface.as_ref() != Some(surface)
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        }
        true
    }
}

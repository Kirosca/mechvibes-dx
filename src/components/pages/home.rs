use crate::components::logo::Logo;
use crate::components::soundpack_selector::{ KeyboardSoundpackSelector, MouseSoundpackSelector };
use crate::components::ui::Toggler;
use crate::components::volume_slider::{ KeyboardVolumeSlider, MouseVolumeSlider };
use crate::libs::AudioContext;
use crate::utils::config::use_config;
use crate::libs::tray_service::request_tray_update;
use dioxus::prelude::*;
use futures_timer::Delay;
use lucide_dioxus::ExternalLink;
use std::sync::atomic::{ AtomicU64, Ordering };
use std::sync::Arc;
use std::time::Duration;

#[component]
pub fn HomePage(audio_ctx: Arc<AudioContext>) -> Element {
    // Use shared config hook
    let (config, update_config) = use_config();

    // Volume states from config
    let mut volume = use_signal(|| config().volume);
    let mut mouse_volume = use_signal(|| config().mouse_volume);
    let enable_sound = use_memo(move || config().enable_sound);

    // Use atomic counters to track save tasks and cancel old ones
    let save_counter = use_signal(|| Arc::new(AtomicU64::new(0)));
    let mouse_save_counter = use_signal(|| Arc::new(AtomicU64::new(0)));

    // Update audio system volume when the volume control changes (enable_sound is handled by sound_manager)
    let ctx = audio_ctx.clone();
    use_effect(move || {
        ctx.set_volume(volume());
    });

    // Update audio system mouse volume when the mouse volume control changes
    let ctx = audio_ctx.clone();
    use_effect(move || {
        ctx.set_mouse_volume(mouse_volume());
    });

    // Debounce effect for saving keyboard volume config changes.
    //
    // The debounce only defers the *disk* write. `spawn` registers the task in
    // this scope's `spawned_tasks`, and navigating to another tab unmounts
    // HomePage, so `Runtime::remove_scope` cancels the task mid-`Delay` and the
    // deferred write never happens. Publishing to the shared config signal
    // therefore has to be synchronous - otherwise the signal keeps the
    // pre-drag volume and re-seeds the slider with it when Home remounts.
    {
        let update_config = update_config.clone();
        use_effect(move || {
            let current_volume = volume();

            // Increment counter to invalidate previous save tasks
            let current_task_id = save_counter().fetch_add(1, Ordering::SeqCst) + 1;

            let update_config = update_config.clone();
            let save_counter_clone = save_counter();

            // Publish immediately so the value survives an unmount; the write
            // itself is deduplicated by `update_config` when nothing changed.
            update_config(
                Box::new(move |config| {
                    config.volume = current_volume;
                })
            );

            spawn(async move {
                // Wait for 500ms
                Delay::new(Duration::from_millis(500)).await;

                // Check if this task is still the latest one
                if save_counter_clone.load(Ordering::SeqCst) == current_task_id {
                    // This is still the latest task, save the config
                    update_config(
                        Box::new(move |config| {
                            config.volume = current_volume;
                        })
                    );
                }
                // If not the latest, this task was "cancelled" by a newer one
            });
        });
    }

    // Debounce effect for saving mouse volume config changes. Same reasoning as
    // the keyboard volume above.
    {
        let update_config = update_config.clone();
        use_effect(move || {
            let current_mouse_volume = mouse_volume();

            // Increment counter to invalidate previous save tasks
            let current_task_id = mouse_save_counter().fetch_add(1, Ordering::SeqCst) + 1;

            let update_config = update_config.clone();
            let mouse_save_counter_clone = mouse_save_counter();

            update_config(
                Box::new(move |config| {
                    config.mouse_volume = current_mouse_volume;
                })
            );

            spawn(async move {
                // Wait for 500ms
                Delay::new(Duration::from_millis(500)).await;

                // Check if this task is still the latest one
                if mouse_save_counter_clone.load(Ordering::SeqCst) == current_task_id {
                    // This is still the latest task, save the config
                    update_config(
                        Box::new(move |config| {
                            config.mouse_volume = current_mouse_volume;
                        })
                    );
                }
                // If not the latest, this task was "cancelled" by a newer one
            });
        });
    }

    rsx! {
      div { class: "flex flex-col gap-10 px-3 pb-0",
        div { class: "mb-2 mt-4",
          // Mechvibes logo with animated press effect
          Logo {}
        }
        // Main content for home page
        div { class: "flex flex-col {crate::utils::spacing::GAP_SPACING}",
          div { class: "{crate::utils::spacing::SECTION_SPACING}",
            KeyboardSoundpackSelector {}
            KeyboardVolumeSlider {
              volume,
              on_change: move |new_volume: f32| {
                  volume.set(new_volume);
              },
            }
          }
          div { class: "divider m-0" }
          div { class: "{crate::utils::spacing::SECTION_SPACING}",
            // Mouse soundpack selector and volume control
            MouseSoundpackSelector {}
            MouseVolumeSlider {
              volume: mouse_volume,
              on_change: move |new_mouse_volume: f32| {
                  mouse_volume.set(new_mouse_volume);
              },
            }
          }
          div { class: "divider m-0" }
          div { class: "{crate::utils::spacing::SECTION_SPACING}",
            // Mirrors the Settings toggle of the same name - both write
            // `enable_sound`, so the shared config signal keeps them in step.
            Toggler {
              title: "Enable all sounds".to_string(),
              description: Some("You can also use Ctrl+Alt+M to toggle sound on/off".to_string()),
              checked: enable_sound(),
              on_change: {
                  let update_config = update_config.clone();
                  let audio_ctx = audio_ctx.clone();
                  move |new_value: bool| {
                      // The engine caches this flag; a config write alone
                      // would leave it playing until restart. Go through the
                      // audio context so the engine is notified too.
                      audio_ctx.set_sound_enabled(new_value);
                      update_config(
                          Box::new(move |config| {
                              config.enable_sound = new_value;
                          }),
                      );
                      request_tray_update();
                  }
              },
            }
            // Same label-left / control-right shape as Toggler above, which is
            // a plain `label` rather than a component, so the row is written
            // out here instead of reaching for one that does not exist.
            div { class: "label w-full justify-between",
              div { class: "space-y-0",
                div { class: "text-sm font-medium text-base-content", "Sound pack editor" }
                div { class: "text-xs whitespace-break-spaces text-base-content/70",
                  "Create and edit your own sound packs on the web"
                }
              }
              div {
                a {
                  class: "btn btn-soft btn-sm",
                  href: "https://mechvibes.com/editor?utm_source=mechvibes&utm_medium=app&utm_campaign=home",
                  target: "_blank",
                  "Open editor"
                  ExternalLink { class: "w-4 h-4 ml-1" }
                }
              }
            }
          }
        }
      }
    }
}

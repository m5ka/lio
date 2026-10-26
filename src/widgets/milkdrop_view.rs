//! The "Visualizer" view for Now Playing: the MilkDrop picture ([`LioMilkdrop`]) with
//! controls over it, a menu of visualizations (by category) and a switch for changing
//! them automatically. The controls show while the pointer is over the view, the menu is
//! open or keyboard focus is inside, and for a few seconds after a tap on touch screens.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{CompositeTemplate, gio, glib};

use crate::application::LioApplication;
use crate::widgets::milkdrop::LioMilkdrop;

/// How long the controls stay after a tap on a touch screen.
const TOUCH_SHOW: Duration = Duration::from_secs(3);

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/dev/m5ka/Lio/ui/milkdrop_view.ui")]
    pub struct LioMilkdropView {
        #[template_child]
        pub milkdrop: TemplateChild<LioMilkdrop>,
        #[template_child]
        pub controls_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub preset_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub preset_label: TemplateChild<gtk::Label>,
        pub motion: RefCell<Option<gtk::EventControllerMotion>>,
        pub focus: RefCell<Option<gtk::EventControllerFocus>>,
        pub preset_action: RefCell<Option<gio::SimpleAction>>,
        /// Shown by a tap, until `touch_timeout` runs out.
        pub touch_shown: Cell<bool>,
        pub touch_timeout: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioMilkdropView {
        const NAME: &'static str = "LioMilkdropView";
        type Type = super::LioMilkdropView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            LioMilkdrop::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LioMilkdropView {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            if let Some(source) = self.touch_timeout.take() {
                source.remove();
            }
        }
    }

    impl WidgetImpl for LioMilkdropView {}
    impl BinImpl for LioMilkdropView {}
}

glib::wrapper! {
    pub struct LioMilkdropView(ObjectSubclass<imp::LioMilkdropView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LioMilkdropView {
    fn setup(&self) {
        let imp = self.imp();
        let milkdrop = imp.milkdrop.get();
        let settings = LioApplication::default().settings().clone();

        // Changing automatically: the toggle writes the setting, the picture follows it.
        settings
            .bind("visualizer-cycle", &milkdrop, "cycle")
            .get()
            .build();
        // Not changing automatically: start where the user left off.
        let last = settings.string("visualizer-preset");
        if !settings.boolean("visualizer-cycle") && !last.is_empty() {
            milkdrop.set_preset(&last);
        }

        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&settings.create_action("visualizer-cycle"));
        let preset = gio::SimpleAction::new_stateful(
            "preset",
            Some(glib::VariantTy::STRING),
            &milkdrop.preset().to_variant(),
        );
        preset.connect_change_state(glib::clone!(
            #[weak]
            milkdrop,
            move |_, value| {
                if let Some(name) = value.and_then(|v| v.get::<String>()) {
                    milkdrop.set_preset(&name);
                }
            }
        ));
        actions.add_action(&preset);
        self.insert_action_group("visualizer", Some(&actions));
        imp.preset_action.replace(Some(preset));
        imp.preset_button
            .set_menu_model(Some(&preset_menu(&milkdrop.preset_names())));

        milkdrop.connect_preset_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.preset_changed()
        ));
        self.preset_changed();

        self.setup_reveal();
        self.update_reveal();
    }

    /// Shows the new preset's name and remembers it.
    fn preset_changed(&self) {
        let imp = self.imp();
        let name = imp.milkdrop.preset();
        if let Some(action) = imp.preset_action.borrow().as_ref() {
            action.set_state(&name.to_variant());
        }
        let (category, title) = split_name(&name);
        imp.preset_label.set_label(title);
        imp.preset_button.set_tooltip_text(Some(&match category {
            Some(category) => format!("{category}: {title}"),
            None => title.to_owned(),
        }));
        imp.preset_button
            .update_property(&[gtk::accessible::Property::Label(
                &gettext("Visualization: {}").replacen("{}", title, 1),
            )]);
        if !name.is_empty() {
            let _ = LioApplication::default()
                .settings()
                .set_string("visualizer-preset", &name);
        }
    }

    /// Shows the controls on hover, focus, an open menu or a tap; hides them otherwise.
    fn setup_reveal(&self) {
        let imp = self.imp();
        let update = glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || view.update_reveal()
        );

        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let update = update.clone();
            move |_, _, _| update()
        });
        motion.connect_leave({
            let update = update.clone();
            move |_| update()
        });
        self.add_controller(motion.clone());
        imp.motion.replace(Some(motion));

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter({
            let update = update.clone();
            move |_| update()
        });
        focus.connect_leave({
            let update = update.clone();
            move |_| update()
        });
        self.add_controller(focus.clone());
        imp.focus.replace(Some(focus));

        // Leaving for the menu's popover counts as leaving the view: keep the controls
        // while it is open, and check again once it closes.
        imp.preset_button.connect_active_notify({
            let update = update.clone();
            move |_| update()
        });

        // Touch screens have no hover: a tap shows the controls for a while.
        let tap = gtk::GestureClick::new();
        tap.set_touch_only(true);
        tap.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _, _| view.show_after_tap()
        ));
        self.add_controller(tap);
    }

    fn show_after_tap(&self) {
        let imp = self.imp();
        imp.touch_shown.set(true);
        if let Some(source) = imp.touch_timeout.take() {
            source.remove();
        }
        let source = glib::timeout_add_local_once(
            TOUCH_SHOW,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().touch_timeout.take();
                    view.imp().touch_shown.set(false);
                    view.update_reveal();
                }
            ),
        );
        imp.touch_timeout.replace(Some(source));
        self.update_reveal();
    }

    fn update_reveal(&self) {
        let imp = self.imp();
        let hovered = imp
            .motion
            .borrow()
            .as_ref()
            .is_some_and(|m| m.contains_pointer());
        let focused = imp
            .focus
            .borrow()
            .as_ref()
            .is_some_and(|f| f.contains_focus());
        // Debug builds: `LIO_TEST_SHOW_CONTROLS=1` keeps them showing (screenshots).
        let forced = cfg!(debug_assertions) && std::env::var_os("LIO_TEST_SHOW_CONTROLS").is_some();
        let reveal =
            forced || hovered || focused || imp.preset_button.is_active() || imp.touch_shown.get();
        imp.controls_revealer.set_reveal_child(reveal);
    }
}

/// `"Fractal - Some Author - Title.milk"` → `(Some("Fractal"), "Some Author - Title")`.
fn split_name(file_name: &str) -> (Option<&str>, &str) {
    let name = file_name.strip_suffix(".milk").unwrap_or(file_name);
    match name.split_once(" - ") {
        Some((category, title)) => (Some(category), title),
        None => (None, name),
    }
}

/// The presets as radio items for `visualizer.preset`, in a submenu per category.
fn preset_menu(names: &[String]) -> gio::Menu {
    let mut by_category: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for name in names {
        let (category, title) = split_name(name);
        by_category
            .entry(category.unwrap_or_default())
            .or_default()
            .push((title, name));
    }
    let menu = gio::Menu::new();
    for (category, presets) in by_category {
        let section = gio::Menu::new();
        for (title, name) in presets {
            let item = gio::MenuItem::new(Some(title), None);
            item.set_action_and_target_value(Some("visualizer.preset"), Some(&name.to_variant()));
            section.append_item(&item);
        }
        if category.is_empty() {
            menu.append_section(None, &section);
        } else {
            menu.append_submenu(Some(category), &section);
        }
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_into_category_and_title() {
        assert_eq!(
            split_name("Fractal - suksma - dimensional drift.milk"),
            (Some("Fractal"), "suksma - dimensional drift")
        );
        assert_eq!(split_name("loose.milk"), (None, "loose"));
    }

    #[test]
    fn menu_groups_presets_by_category() {
        let names = ["Fractal - a.milk", "Hypnotic - b.milk", "Fractal - c.milk"].map(String::from);
        let menu = preset_menu(&names);
        assert_eq!(menu.n_items(), 2);
        let label = |i| {
            menu.item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
        };
        assert_eq!(label(0).as_deref(), Some("Fractal"));
        assert_eq!(label(1).as_deref(), Some("Hypnotic"));
        let fractal = menu.item_link(0, "submenu").unwrap();
        assert_eq!(fractal.n_items(), 2);
    }
}

/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use crate::api_parser::Class;
use crate::{codegen_special_cases, util, ExtensionApi, GodotTy, RustTy, TyName};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, ToTokens};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct Context<'a> {
    engine_classes: HashMap<TyName, &'a Class>,
    builtin_types: HashSet<&'a str>,
    native_structures_types: HashSet<&'a str>,
    singletons: HashSet<&'a str>,
    inheritance_tree: InheritanceTree,
    cached_rust_types: HashMap<GodotTy, RustTy>,
    notifications_by_class: HashMap<TyName, Vec<(Ident, i32)>>,
    notification_enum_names_by_class: HashMap<TyName, NotificationEnum>,
}

impl<'a> Context<'a> {
    pub fn build_from_api(api: &'a ExtensionApi) -> Self {
        let mut ctx = Self::default();

        for class in api.singletons.iter() {
            ctx.singletons.insert(class.name.as_str());
        }

        ctx.builtin_types.insert("Variant"); // not part of builtin_classes
        for builtin in api.builtin_classes.iter() {
            let ty_name = builtin.name.as_str();
            ctx.builtin_types.insert(ty_name);
        }

        for structure in api.native_structures.iter() {
            let ty_name = structure.name.as_str();
            ctx.native_structures_types.insert(ty_name);
        }

        for class in api.classes.iter() {
            let class_name = TyName::from_godot(&class.name);

            if codegen_special_cases::is_class_excluded(class_name.godot_ty.as_str()) {
                continue;
            }

            // Populate class lookup by name
            println!("-- add engine class {}", class_name.description());
            ctx.engine_classes.insert(class_name.clone(), class);

            // Populate derived-to-base relations
            if let Some(base) = class.inherits.as_ref() {
                let base_name = TyName::from_godot(base);
                println!("  -- inherits {}", base_name.description());
                ctx.inheritance_tree.insert(class_name.clone(), base_name);
            }

            // Populate notification constants (first, only for classes that declare them themselves).
            if let Some(constants) = class.constants.as_ref() {
                let mut has_notifications = false;

                for constant in constants.iter() {
                    if let Some(rust_constant) = util::try_to_notification(constant) {
                        // First time
                        if !has_notifications {
                            ctx.notifications_by_class
                                .insert(class_name.clone(), Vec::new());

                            ctx.notification_enum_names_by_class.insert(
                                class_name.clone(),
                                NotificationEnum::for_own_class(&class_name),
                            );

                            has_notifications = true;
                        }

                        ctx.notifications_by_class
                            .get_mut(&class_name)
                            .expect("just inserted constants; must be present")
                            .push((rust_constant, constant.value));
                    }
                }
            }
        }

        // Populate remaining notification enum names, by copying the one to nearest base class that has at least 1 notification.
        // At this point all classes with notifications are registered.
        // (Used to avoid re-generating the same notification enum for multiple base classes).
        for class_name in ctx.engine_classes.keys() {
            if ctx
                .notification_enum_names_by_class
                .contains_key(class_name)
            {
                continue;
            }

            let all_bases = ctx.inheritance_tree.collect_all_bases(class_name);

            let mut nearest = None;
            for (i, elem) in all_bases.iter().enumerate() {
                if let Some(nearest_enum_name) = ctx.notification_enum_names_by_class.get(elem) {
                    nearest = Some((i, nearest_enum_name.clone()));
                    break;
                }
            }
            let (nearest_index, nearest_enum_name) =
                nearest.expect("at least one base must have notifications");

            // For all bases inheriting most-derived base that has notification constants, reuse the type name.
            for i in (0..nearest_index).rev() {
                let base_name = &all_bases[i];
                let enum_name = NotificationEnum::for_other_class(nearest_enum_name.clone());

                ctx.notification_enum_names_by_class
                    .insert(base_name.clone(), enum_name);
            }

            // Also for this class, reuse the type name.
            let enum_name = NotificationEnum::for_other_class(nearest_enum_name);

            ctx.notification_enum_names_by_class
                .insert(class_name.clone(), enum_name);
        }

        ctx
    }

    pub fn get_engine_class(&self, class_name: &TyName) -> &Class {
        self.engine_classes.get(class_name).unwrap()
    }

    // pub fn is_engine_class(&self, class_name: &str) -> bool {
    //     self.engine_classes.contains(class_name)
    // }

    /// Checks if this is a builtin type (not `Object`).
    ///
    /// Note that builtins != variant types.
    pub fn is_builtin(&self, ty_name: &str) -> bool {
        self.builtin_types.contains(ty_name)
    }

    pub fn is_native_structure(&self, ty_name: &str) -> bool {
        self.native_structures_types.contains(ty_name)
    }

    pub fn is_singleton(&self, class_name: &str) -> bool {
        self.singletons.contains(class_name)
    }

    pub fn is_exportable(&self, class_name: &TyName) -> bool {
        if class_name.godot_ty == "Resource" || class_name.godot_ty == "Node" {
            return true;
        }

        self.inheritance_tree
            .collect_all_bases(class_name)
            .iter()
            .any(|ty| ty.godot_ty == "Resource" || ty.godot_ty == "Node")
    }

    pub fn inheritance_tree(&self) -> &InheritanceTree {
        &self.inheritance_tree
    }

    pub fn find_rust_type(&'a self, ty: &GodotTy) -> Option<&'a RustTy> {
        self.cached_rust_types.get(ty)
    }

    pub fn notification_constants(&'a self, class_name: &TyName) -> Option<&Vec<(Ident, i32)>> {
        self.notifications_by_class.get(class_name)
    }

    pub fn notification_enum_name(&self, class_name: &TyName) -> NotificationEnum {
        self.notification_enum_names_by_class
            .get(class_name)
            .unwrap_or_else(|| panic!("class {} has no notification enum name", class_name.rust_ty))
            .clone()
    }

    pub fn insert_rust_type(&mut self, godot_ty: GodotTy, resolved: RustTy) {
        let prev = self.cached_rust_types.insert(godot_ty, resolved);
        assert!(prev.is_none(), "no overwrites of RustTy");
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct NotificationEnum {
    /// Name of the enum.
    pub name: Ident,

    /// Whether this is declared by the current class (from context), rather than inherited.
    pub declared_by_own_class: bool,
}

impl NotificationEnum {
    fn for_own_class(class_name: &TyName) -> Self {
        Self {
            name: format_ident!("{}Notification", class_name.rust_ty),
            declared_by_own_class: true,
        }
    }

    fn for_other_class(other: NotificationEnum) -> Self {
        Self {
            name: other.name,
            declared_by_own_class: false,
        }
    }

    /// Returns the name of the enum if it is declared by the current class, or `None` if it is inherited.
    pub fn try_to_own_name(&self) -> Option<Ident> {
        if self.declared_by_own_class {
            Some(self.name.clone())
        } else {
            None
        }
    }
}

impl ToTokens for NotificationEnum {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.name.to_tokens(tokens)
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------------------

/// Maintains class hierarchy. Uses Rust class names, not Godot ones.
#[derive(Default)]
pub(crate) struct InheritanceTree {
    derived_to_base: HashMap<TyName, TyName>,
}

impl InheritanceTree {
    pub fn insert(&mut self, derived_name: TyName, base_name: TyName) {
        let existing = self.derived_to_base.insert(derived_name, base_name);
        assert!(existing.is_none(), "Duplicate inheritance insert");
    }

    /// Returns all base classes, without the class itself, in order from nearest to furthest (object).
    pub fn collect_all_bases(&self, derived_name: &TyName) -> Vec<TyName> {
        let mut maybe_base = derived_name;
        let mut result = vec![];

        while let Some(base) = self.derived_to_base.get(maybe_base) {
            result.push(base.clone());
            maybe_base = base;
        }
        result
    }
}

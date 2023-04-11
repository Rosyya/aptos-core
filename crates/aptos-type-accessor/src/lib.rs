// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use anyhow::{bail, Context, Result};
use aptos_api_types::{MoveModule, MoveType};
use aptos_rest_client::Client as RestClient;
use move_binary_format::file_format::CompiledModule;
use move_core_types::{
    identifier::Identifier,
    language_storage::{ModuleId},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

struct TypeAccessorBuilder {
    modules_to_retrieve: BTreeSet<ModuleId>,
    modules: BTreeMap<ModuleId, MoveModule>,
    recurse: bool,
    rest_client: Option<Arc<RestClient>>,
}

impl TypeAccessorBuilder {
    pub fn new() -> Self {
        Self {
            modules_to_retrieve: BTreeSet::new(),
            modules: BTreeMap::new(),
            recurse: true,
            rest_client: None,
        }
    }

    /// Add the client that we'll use for the lookups. This must be provided if
    /// we're going to do lookups.
    // TODO: Consider using this: https://github.com/idanarye/rust-typed-builder
    // Or just the typed builder pattern, where one builder becomes another builder.
    pub fn rest_client(mut self, rest_client: Arc<RestClient>) -> Self {
        self.rest_client = Some(rest_client);
        self
    }

    /// Add modules that will be looked up when building the TypeAccessor.
    pub fn lookup_modules(mut self, module_ids: Vec<ModuleId>) -> Self {
        self.modules_to_retrieve.extend(module_ids);
        self
    }

    /// Add a module that will be looked up when building the TypeAccessor.
    pub fn lookup_module(self, module_id: ModuleId) -> Self {
        self.lookup_modules(vec![module_id])
    }

    /// Add modules that we already have.
    pub fn add_modules(mut self, modules: Vec<MoveModule>) -> Self {
        for module in modules {
            self.modules.insert(
                ModuleId::new(module.address.into(), module.name.clone().into()),
                module,
            );
        }
        self
    }

    /// Add a module that we already have.
    pub fn add_module(self, module: MoveModule) -> Self {
        self.add_modules(vec![module])
    }

    /// If set, do not look up modules as needed if they appear while building the
    /// TypeAccessor.
    pub fn do_not_recurse(mut self) -> Self {
        self.recurse = false;
        self
    }

    pub async fn build(mut self) -> anyhow::Result<TypeAccessor> {
        if self.modules_to_retrieve.is_empty() && self.modules.is_empty() {
            bail!("Cannot build TypeAccessor without any modules to lookup or add");
        }
        if !self.modules_to_retrieve.is_empty() && self.rest_client.is_none() {
            bail!("Cannot build TypeAccessor without an API client if we need to lookup modules");
        }

        let mut field_info: BTreeMap<
            ModuleId,
            BTreeMap<Identifier, BTreeMap<Identifier, MoveType>>,
        > = BTreeMap::new();

        // let mut modules_processed = BTreeSet::new();

        loop {
            if !self.modules_to_retrieve.is_empty() {
                // If there are modules to lookup, do that.
                while let Some(module_id) = self.modules_to_retrieve.pop_first() {
                    // TODO: Use batch get.
                    if self.modules.contains_key(&module_id) {
                        continue;
                    }
                    self.modules
                        .insert(module_id.clone(), self.retrieve_module(module_id).await?);
                }
            } else if !self.modules.is_empty() {
                // We have no modules to retrieve right now, let's parse modules if we have them.
                while let Some((module_id, module)) = self.modules.pop_first() {
                    let (structs_info, modules_to_retrieve) =
                        self.parse_module(module);

                    field_info.insert(module_id, structs_info);

                    self.modules_to_retrieve.extend(modules_to_retrieve);
                }
            } else {
                // We have no modules to retrieve and no modules to parse, we're done.
                break;
            }
        }

        Ok(TypeAccessor::new(field_info))
    }

    async fn retrieve_module(&self, module_id: ModuleId) -> Result<MoveModule> {
        let module_bytecode = self
            .rest_client
            .as_ref()
            .unwrap()
            .get_account_module_bcs(*module_id.address(), module_id.name().as_str())
            .await
            .context(format!(
                "Failed to get module {}::{}",
                module_id.address(),
                module_id.name()
            ))?
            .into_inner();

        let module: MoveModule = CompiledModule::deserialize(&module_bytecode)
            .context(format!(
                "Failed to deserialize module {}::{}",
                module_id.address(),
                module_id.name()
            ))?
            .into();

        Ok(module)
    }

    fn parse_module(
        &self,
        module: MoveModule,
    ) -> (
        // The map of struct to field to field type.
        BTreeMap<Identifier, BTreeMap<Identifier, MoveType>>,
        // Any new Move modules we need to retrieve.
        BTreeSet<ModuleId>,
    ) {
        let mut structs_info = BTreeMap::new();
        let mut modules_to_retrieve = BTreeSet::new();

        // For each struct in the module look through the types of the fields and
        // determine any more modules we need to look up.
        for struc in module.structs.into_iter() {
            let mut types_to_resolve = Vec::new();
            let mut types_seen = HashSet::new();

            for field in struc.fields {
                types_to_resolve.push(field.typ.clone());
                structs_info
                    .entry(struc.name.clone().into())
                    .or_insert_with(BTreeMap::new)
                    .insert(field.name.into(), field.typ);
            }

            // Go through the types recursively until we hit leaf types. As we do so,
            // we add more modules to `modules_to_retrieve`. This way, we can ensure
            // that we look up the types for all modules relevant to this struct.
            if self.recurse {
                while let Some(typ) = types_to_resolve.pop() {
                    if types_seen.contains(&typ) {
                        continue;
                    }
                    types_seen.insert(typ.clone());

                    // For types that refer to other types, add those to the list of
                    // types. This continues until we hit leaves / a cycle.
                    match typ {
                        MoveType::Vector { items: typ } => {
                            types_to_resolve.push(*typ);
                        },
                        MoveType::Reference {
                            mutable: _,
                            to: typ,
                        } => {
                            types_to_resolve.push(*typ);
                        },
                        MoveType::Struct(struct_tag) => {
                            modules_to_retrieve.insert(ModuleId::new(
                                struct_tag.address.into(),
                                struct_tag.module.into(),
                            ));
                        },
                        other => {},
                    }
                }
            }
        }

        (structs_info, modules_to_retrieve)
    }
}

/// TypeAccessor is a utility for looking up the types of fields in a resource.
struct TypeAccessor {
    /// This is a map of ModuleId (address, name) to a map of struct name to a map of field name to field type.
    field_info: BTreeMap<
        // Module address + name.
        ModuleId,
        BTreeMap<
            // Struct name.
            Identifier,
            BTreeMap<
                // Field name.
                Identifier,
                // Field type.
                MoveType,
            >,
        >,
    >,
}

impl TypeAccessor {
    pub fn new(
        field_info: BTreeMap<ModuleId, BTreeMap<Identifier, BTreeMap<Identifier, MoveType>>>,
    ) -> Self {
        Self { field_info }
    }
}
/*

impl TypeAccessor {
    // This is a function that recursively builds up a map of field types.
    // We assume here that the user only cares about the types of fields.
    async fn build(module_id: ModuleId) -> Self {
        let mut field_info = BTreeMap::new();

        let mut modules_to_resolve = vec![module_id];
        let mut modules_seen = BTreeSet::new();

        // Start with the top level module.
        while let Some(module_id) = modules.pop() {
            if modules_seen.contains(module_id) {
                continue;
            }
            modules_seen.insert(module_id);

            let module: MoveModule = rest_client.get_module(address, name).await;
            let (address, name) = module_id;

            // For each struct in the module look through the types of the fields and
            // determine any more modules we need to look up.
            for struc in module.structs {
                let mut types_to_resolve = Vec::new();
                let mut types_seen = BTreeSet::new();

                for field in struc.fields {
                    types_to_resolve.push(field.typ);
                    field_info.insert((address, name, struc.name, field.name), field.typ);
                }

                // Go through the types recursively, adding more modules to
                // `modules_to_resolve`.
                while let Some(typ) = types_to_resolve.pop() {
                    if types_seen.contains(typ) {
                        continue;
                    }
                    types_seen.insert(typ);

                    // For types that refer to other types, add those to the list of
                    // types. This continues until we hit leaves / a cycle.
                    match typ {
                        MoveType::Vector { typ } => {
                            types_to_resolve.push(typ);
                        },
                        MoveType::Reference { _mutable, typ } => {
                            types_to_resolve.push(typ);
                        },
                        MoveType::Struct(struct_tag) => {
                            modules_to_resolve.push((struct_tag.address, struct_tag.module));
                        },
                        other => {},
                    }
                }
            }
        }

        Self {
            module_id,
            field_info,
        }
    }

    fn get_type(access_path: AccessPath) -> MoveType {
        todo!();
    }
}
*/

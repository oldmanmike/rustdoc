//! Functions used to generate the documentation for Rust Crates.

#![warn(missing_docs)]

extern crate rls_analysis as analysis;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
#[macro_use]
extern crate error_chain;
extern crate indicatif;

pub mod error;
pub use error::{Error, ErrorKind};

use error::*;

pub mod item;
use item::Metadata;

pub mod json;
use json::*;

use std::collections::HashMap;
use std::fs::{self, File, DirBuilder};
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

use analysis::AnalysisHost;
use analysis::raw::DefKind;
use indicatif::ProgressBar;

/// A structure that contains various fields that hold data in order to generate doc output.
///
/// ## Fields
///
/// - `manifest_path`: Path to the directory with the `Cargo.toml` file for the crate being analyzed
/// - `host`: Contains the Cargo analysis output for the crate being documented
/// - `assets`: Contains all of the `Asset`s that will be output at the end (e.g. JSON, CSS, HTML
///             and/or JS)
pub struct Config {
    manifest_path: PathBuf,
    host: analysis::AnalysisHost,
    assets: Vec<Asset>,
}

/// Static assets compiled into the binary so we get a single executable. These are dynamically
/// generated with the build script based off of items in the `frontend/dist` folder.
///
/// ## Fields
///
/// - `name`: Name of the file loaded into the binary
/// - `contents`: Content of the files being loaded into the binary
struct Asset {
    name: &'static str,
    contents: &'static str,
}

impl Config {
    /// Create a new `Config` based off the location of the manifest as well as assets generated
    /// during the build phase
    ///
    /// ## Arguments
    ///
    /// - manifest_path: The path to the location of `Cargo.toml` of the crate being documented
    pub fn new(manifest_path: PathBuf) -> Result<Config> {
        let host = analysis::AnalysisHost::new(analysis::Target::Debug);

        let assets = include!("asset.in");

        Ok(Config {
            manifest_path,
            host,
            assets,
        })
    }
}

/// Generate documentation for a crate. This can be tuned to output JSON and/or Web assets to view
/// documentation or use the JSON for other applications built on top of `rustdoc`.
///
/// ## Arguments
///
/// - config: The `Config` struct that contains the data needed to generate the documentation
/// - artifacts: A slice containing what assets should be output at the end
pub fn build(config: &Config, artifacts: &[&str]) -> Result<()> {
    generate_analysis(config)?;

    let package_name = crate_name_from_manifest_path(&config.manifest_path)?;
    let data = DocData::new(&config.host, &package_name)?;

    let output_path = config.manifest_path.join("target/doc");
    fs::create_dir_all(&output_path)?;

    if artifacts.contains(&"json") {
        let spinner = ProgressBar::new_spinner();
        spinner.enable_steady_tick(50);
        spinner.set_message("Generating JSON: In Progress");

        let json = data.to_json()?;

        let mut json_path = output_path.clone();
        json_path.push("data.json");

        let mut file = File::create(json_path)?;
        file.write_all(json.as_bytes())?;
        spinner.finish_with_message("Generating JSON: Done");
    }

    // now that we've written out the data, we can write out the rest of it
    if artifacts.contains(&"assets") {
        let spinner = ProgressBar::new_spinner();
        spinner.enable_steady_tick(50);
        spinner.set_message("Copying Assets: In Progress");

        let mut assets_path = output_path.clone();
        assets_path.push("assets");
        fs::create_dir_all(&assets_path)?;

        for asset in &config.assets {
            create_asset_file(asset.name, &output_path, asset.contents)?;
        }

        spinner.finish_with_message("Copying Assets: Done");
    }

    Ok(())
}

/// Grab the name of the binary or library from it's `Cargo.toml` file.
///
/// ## Arguments
///
/// - manifest_path: The path to the location of `Cargo.toml` of the crate being documented
fn crate_name_from_manifest_path(manifest_path: &Path) -> Result<String> {
    let mut command = Command::new("cargo");

    command
        .arg("metadata")
        .arg("--manifest-path")
        .arg(manifest_path.join("Cargo.toml"))
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1");

    let output = command.output()?;

    if !output.status.success() {
        return Err(
            ErrorKind::Cargo(
                output.status,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ).into(),
        );
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let targets = match json["packages"][0]["targets"].as_array() {
        Some(targets) => targets,
        None => return Err(ErrorKind::Json("targets is not an array").into()),
    };

    for target in targets {
        let crate_types = match target["crate_types"].as_array() {
            Some(crate_types) => crate_types,
            None => return Err(ErrorKind::Json("crate types is not an array").into()),
        };

        for crate_type in crate_types {

            let ty = match crate_type.as_str() {
                Some(t) => t,
                None => {
                    return Err(
                        ErrorKind::Json("crate type contents are not a string").into(),
                    )
                }
            };

            if ty == "lib" {
                match target["name"].as_str() {
                    Some(name) => return Ok(name.to_string()),
                    None => return Err(ErrorKind::Json("target name is not a string").into()),
                }
            }
        }
    }

    Err(ErrorKind::Json("cargo metadata").into())
}

/// Output an asset file to a given directory
///
/// ## Arguments
///
/// - name: Name of the asset file
/// - path: Path to the directory to write the file out to
/// - data: Data to be written to the file
fn create_asset_file(name: &str, path: &Path, data: &str) -> Result<()> {
    let mut asset_path = path.to_path_buf();
    asset_path.push(name);

    // the name may contain one or more directories. we need to create them before trying to create
    // a file
    if let Some(parent) = asset_path.parent() {
        if parent != path {
            DirBuilder::new().recursive(true).create(parent)?;
        }
    }

    let mut file = File::create(asset_path)?;
    file.write_all(data.as_bytes())?;

    Ok(())
}

/// Generate save analysis data of a crate to be used later by the RLS library later
///
/// ## Arguments:
///
/// - config: Contains data for what needs to be output or used. In this case the path to the
///           `Cargo.toml` file
fn generate_analysis(config: &Config) -> Result<()> {
    let mut command = Command::new("cargo");
    let manifest_path = &config.manifest_path;

    command
        .arg("check")
        .arg("--manifest-path")
        .arg(manifest_path.join("Cargo.toml"))
        .env("RUSTFLAGS", "-Z save-analysis")
        .env("CARGO_TARGET_DIR", manifest_path.join("target/rls"));

    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(50);
    spinner.set_message("Generating save analysis data: In Progress");

    let output = command.output()?;

    if !output.status.success() {
        spinner.finish_with_message("Generating save analysis data: Error");
        return Err(
            ErrorKind::Cargo(
                output.status,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ).into(),
        );
    }
    spinner.finish_with_message("Generating save analysis data: Done");

    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(50);
    spinner.set_message("Loading save analysis data: In Progress");
    config.host.reload(manifest_path, manifest_path, true)?;
    spinner.finish_with_message("Loading save analysis data: Done");

    Ok(())
}

/// Documentation data generated for a crate
///
/// ## Fields
///
/// - id: Contains the unique identifier used for the crate that's used by RLS
/// - name: The name of the crate being documented
/// - docs: The documentation string for the top level crate docs
/// - metadata: Values representing all of the data that is documented in the crate like functions,
///             enums, types, etc.
#[derive(Debug)]
pub struct DocData {
    id: analysis::Id,
    name: String,
    docs: String,
    metadata: Vec<Metadata>,
}

impl DocData {
    /// Create a new `DocData` for the package given
    ///
    /// ## Arguments
    ///
    /// - host: Contains the analysis of a crate generated by `Cargo`
    /// - crate_name: The name for the package. For now if the name isn't `example` we abort
    pub fn new(host: &AnalysisHost, crate_name: &str) -> Result<DocData> {
        let roots = host.def_roots()?;

        let id = roots.iter().find(|&&(_, ref name)| name == &crate_name);
        let root_id = match id {
            Some(&(id, _)) => id,
            _ => return Err(ErrorKind::CrateErr(crate_name.to_string()).into()),
        };

        let root_def = host.get_def(root_id)?;

        let mut doc_data = DocData {
            id: root_id,
            name: root_def.qualname.to_string(),
            docs: root_def.docs.clone(),
            metadata: Vec::new(),
        };

        // TODO: https://github.com/steveklabnik/rustdoc/issues/70
        fn recur(id: &analysis::Id, host: &AnalysisHost) -> Vec<analysis::Def> {
            let defs_and_ids = host.for_each_child_def(*id, |id, def| (id, def.clone()))
                .unwrap();

            let mut v = Vec::new();

            for (id, def) in defs_and_ids.into_iter() {
                v.push(def);

                for def in recur(&id, host).into_iter() {
                    v.push(def);
                }
            }

            v
        }

        let defs: Vec<analysis::Def> = recur(&root_id, host);

        for def in defs.into_iter() {
            match def.kind {
                DefKind::Mod => {
                    doc_data.metadata.push(Metadata::Module {
                        qualified_name: def.qualname,
                        name: def.name,
                        docs: def.docs,
                    });
                }
                DefKind::Static => (),
                DefKind::Const => (),
                DefKind::Enum => (),
                DefKind::Struct => (),
                DefKind::Union => (),
                DefKind::Trait => (),
                DefKind::Function => (),
                DefKind::Macro => (),
                DefKind::Tuple => (),
                DefKind::Method => (),
                DefKind::Type => (),
                DefKind::Local => (),
                DefKind::Field => (),
            }
        }

        Ok(doc_data)
    }

    /// Serialize the data structure into a valid `JSON API` String for output later.
    pub fn to_json(&self) -> Result<String> {

        // Set up the values we'll later push into the to `Documentation` struct to be serialized
        let mut included: Vec<Document> = Vec::new();
        let mut relationships: HashMap<&str, Vec<Data>> = HashMap::with_capacity(METADATA_SIZE);

        // Check each item in the metadata and add it to be serialized based off it's type
        for item in self.metadata.iter() {
            match item {
                &Metadata::Module {
                    ref qualified_name,
                    ref name,
                    ref docs,
                } => {

                    // The `relationships` `HashMap` had a module value added before so we push this
                    // new module relationship into it
                    if let Some(ref mut vec) = relationships.get_mut("modules") {
                        vec.push(Data::new().ty("module").id(&qualified_name));
                    }

                    // We do this to avoid borrow check errors regarding two mutable references.
                    // A "modules" value was never inserted into the HashMap before so we create it
                    if let None = relationships.get("modules") {
                        relationships.insert(
                            "modules",
                            vec![Data::new().ty("module").id(&qualified_name)],
                        );
                    }

                    // Using the module's metadata we create a new `Document` type to be put in the
                    // eventual serialized JSON
                    let module = Document::new()
                        .ty("module")
                        .id(&qualified_name)
                        .attributes("name", name)
                        .attributes("docs", docs);

                    included.push(module);
                }
                _ => {}
            }
        }

        let len = self.name.len();

        // Create the top level crate `Document` for the "data" field in the serialized JSON
        let mut data_document = Document::new()
            .ty("crate")
            .id(&self.name[..(len - 2)])
            .attributes("docs", &self.docs);

        // Insert all of the different types of relationships into this `Document` type only
        for (ty, data) in relationships.into_iter() {
            data_document.relationships(ty, data);
        }

        // Serialize the data and return it
        Ok(serde_json::to_string(
            &Documentation::new().data(data_document).included(
                included,
            ),
        )?)
    }
}

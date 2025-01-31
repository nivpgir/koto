use {
    crate::{Chunk, Compiler, CompilerError, CompilerSettings},
    dunce::canonicalize,
    koto_parser::{format_error_with_excerpt, Parser, ParserError},
    std::{collections::HashMap, error, fmt, path::PathBuf, sync::Arc},
};

/// Errors that can be returned from [Loader] operations
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum LoaderErrorType {
    Parser(ParserError),
    Compiler(CompilerError),
    Io(String),
}

/// The error type used by the [Loader]
#[derive(Clone, Debug)]
pub struct LoaderError {
    error: LoaderErrorType,
    source: String,
    source_path: Option<PathBuf>,
}

impl LoaderError {
    pub(crate) fn from_parser_error(
        error: ParserError,
        source: &str,
        source_path: Option<PathBuf>,
    ) -> Self {
        Self {
            error: LoaderErrorType::Parser(error),
            source: source.into(),
            source_path,
        }
    }

    pub(crate) fn from_compiler_error(
        error: CompilerError,
        source: &str,
        source_path: Option<PathBuf>,
    ) -> Self {
        Self {
            error: LoaderErrorType::Compiler(error),
            source: source.into(),
            source_path,
        }
    }

    pub(crate) fn io_error(error: String) -> Self {
        Self {
            error: LoaderErrorType::Io(error),
            source: "".into(),
            source_path: None,
        }
    }

    /// Returns true if the error was caused by the expectation of indentation during parsing
    pub fn is_indentation_error(&self) -> bool {
        match &self.error {
            LoaderErrorType::Parser(e) => e.is_indentation_error(),
            _ => false,
        }
    }
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use LoaderErrorType::*;

        if f.alternate() {
            match &self.error {
                Parser(koto_parser::ParserError { error, .. }) => f.write_str(&error.to_string()),
                Compiler(crate::CompilerError { message, .. }) => f.write_str(message),
                Io(e) => f.write_str(e),
            }
        } else {
            match &self.error {
                Parser(koto_parser::ParserError { error, span }) => {
                    f.write_str(&format_error_with_excerpt(
                        Some(&error.to_string()),
                        &self.source_path,
                        &self.source,
                        span.start,
                        span.end,
                    ))
                }
                Compiler(crate::CompilerError { message, span }) => {
                    f.write_str(&format_error_with_excerpt(
                        Some(message),
                        &self.source_path,
                        &self.source,
                        span.start,
                        span.end,
                    ))
                }
                Io(e) => f.write_str(e),
            }
        }
    }
}

impl error::Error for LoaderError {}

/// Helper for loading, compiling, and caching Koto modules
#[derive(Clone, Default)]
pub struct Loader {
    chunks: HashMap<PathBuf, Arc<Chunk>>,
}

impl Loader {
    fn compile(
        &mut self,
        script: &str,
        script_path: Option<PathBuf>,
        compiler_settings: CompilerSettings,
    ) -> Result<Arc<Chunk>, LoaderError> {
        match Parser::parse(script) {
            Ok(ast) => {
                let (bytes, mut debug_info) = match Compiler::compile(&ast, compiler_settings) {
                    Ok((bytes, debug_info)) => (bytes, debug_info),
                    Err(e) => return Err(LoaderError::from_compiler_error(e, script, script_path)),
                };

                debug_info.source = script.to_string();

                Ok(Arc::new(Chunk::new(
                    bytes,
                    ast.consume_constants(),
                    script_path,
                    debug_info,
                )))
            }
            Err(e) => Err(LoaderError::from_parser_error(e, script, script_path)),
        }
    }

    /// Compiles a script in REPL mode
    pub fn compile_repl(&mut self, script: &str) -> Result<Arc<Chunk>, LoaderError> {
        self.compile(script, None, CompilerSettings { repl_mode: true })
    }

    /// Compiles a script
    pub fn compile_script(
        &mut self,
        script: &str,
        script_path: &Option<PathBuf>,
    ) -> Result<Arc<Chunk>, LoaderError> {
        self.compile(script, script_path.clone(), CompilerSettings::default())
    }

    /// Finds a module from its name, and then compiles it
    pub fn compile_module(
        &mut self,
        name: &str,
        load_from_path: Option<PathBuf>,
    ) -> Result<(Arc<Chunk>, PathBuf), LoaderError> {
        // Get either the directory of the provided path, or the current working directory
        let path = match &load_from_path {
            Some(path) => match canonicalize(path) {
                Ok(canonicalized) if canonicalized.is_file() => match canonicalized.parent() {
                    Some(parent_dir) => parent_dir.to_path_buf(),
                    None => {
                        return Err(LoaderError::io_error(
                            "Failed to get parent of provided path".to_string(),
                        ))
                    }
                },
                Ok(canonicalized) => canonicalized,
                Err(e) => return Err(LoaderError::io_error(e.to_string())),
            },
            None => match std::env::current_dir() {
                Ok(path) => path,
                Err(e) => return Err(LoaderError::io_error(e.to_string())),
            },
        };

        let mut load_module_from_path = |module_path: PathBuf| match self.chunks.get(&module_path) {
            Some(chunk) => Ok((chunk.clone(), module_path.clone())),
            None => match std::fs::read_to_string(&module_path) {
                Ok(script) => {
                    let chunk = self.compile(
                        &script,
                        Some(module_path.clone()),
                        CompilerSettings::default(),
                    )?;

                    self.chunks.insert(module_path.clone(), chunk.clone());
                    Ok((chunk, module_path))
                }
                Err(_) => Err(LoaderError::io_error(format!(
                    "File not found: {}",
                    module_path.to_string_lossy()
                ))),
            },
        };

        let extension = "koto";
        let named_path = path.join(name);

        // First, check for a neighbouring file with a matching name.
        let module_path = named_path.with_extension(extension);
        if module_path.exists() {
            load_module_from_path(module_path)
        } else {
            // Alternatively, check for a neighbouring directory with a matching name,
            // that also contains a main file.
            let module_path = named_path.join("main").with_extension(extension);
            if module_path.exists() {
                load_module_from_path(module_path)
            } else {
                Err(LoaderError::io_error(format!(
                    "Unable to find module '{}'",
                    name
                )))
            }
        }
    }
}

use {
    koto::{
        runtime::{KotoFile, KotoRead, KotoWrite, RuntimeError},
        Koto, KotoSettings,
    },
    std::{
        fmt,
        sync::{Arc, Mutex},
    },
    wasm_bindgen::prelude::*,
};

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

// Captures output from Koto in a String
#[derive(Debug)]
struct OutputCapture {
    output: Arc<Mutex<String>>,
}

impl KotoFile for OutputCapture {}
impl KotoRead for OutputCapture {}

impl KotoWrite for OutputCapture {
    fn write(&self, bytes: &[u8]) -> Result<(), RuntimeError> {
        let bytes_str = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => return Err(e.to_string().into()),
        };
        self.output.lock().unwrap().push_str(bytes_str);
        Ok(())
    }

    fn write_line(&self, output: &str) -> Result<(), RuntimeError> {
        let mut unlocked = self.output.lock().unwrap();
        unlocked.push_str(output);
        unlocked.push('\n');
        Ok(())
    }

    fn flush(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
}

impl fmt::Display for OutputCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("_stdout_")
    }
}

// Runs an input program and returns the output as a String
#[wasm_bindgen]
pub fn compile_and_run(input: &str) -> String {
    let output = Arc::new(Mutex::new(String::new()));

    let mut koto = Koto::with_settings(KotoSettings {
        stdout: Arc::new(OutputCapture {
            output: output.clone(),
        }),
        stderr: Arc::new(OutputCapture {
            output: output.clone(),
        }),
        ..Default::default()
    });

    match koto.compile(input) {
        Ok(_) => match koto.run() {
            Ok(_) => std::mem::take(&mut output.lock().unwrap()),
            Err(e) => format!("Runtime error: {}", e),
        },
        Err(e) => format!("Compilation error: {}", e),
    }
}

use {
    koto_bytecode::Chunk,
    koto_runtime::{KotoFile, KotoRead, KotoWrite, Loader, Mutex, RuntimeError, Vm, VmSettings},
    std::{fmt, sync::Arc},
};

#[derive(Debug)]
struct TestStdout {
    output: Arc<Mutex<String>>,
}

impl KotoFile for TestStdout {}
impl KotoRead for TestStdout {}

impl KotoWrite for TestStdout {
    fn write(&self, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.output
            .lock()
            .push_str(std::str::from_utf8(bytes).unwrap());
        Ok(())
    }

    fn write_line(&self, s: &str) -> Result<(), RuntimeError> {
        self.output.lock().push_str(s);
        self.output.lock().push('\n');
        Ok(())
    }

    fn flush(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
}

impl fmt::Display for TestStdout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("_teststdout_")
    }
}

mod vm {
    use super::*;

    fn check_logged_output(script: &str, expected_output: &str) {
        let output = Arc::new(Mutex::new(String::new()));

        let mut vm = Vm::with_settings(VmSettings {
            stdout: Arc::new(TestStdout {
                output: output.clone(),
            }),
            stderr: Arc::new(TestStdout {
                output: output.clone(),
            }),
            ..Default::default()
        });

        let print_chunk = |script: &str, chunk: Arc<Chunk>| {
            println!("{}\n", script);
            let script_lines = script.lines().collect::<Vec<_>>();

            println!("Constants\n---------\n{}\n", chunk.constants.to_string());
            println!(
                "Instructions\n------------\n{}",
                Chunk::instructions_as_string(chunk, &script_lines)
            );
        };

        let mut loader = Loader::default();
        let chunk = match loader.compile_script(script, &None) {
            Ok(chunk) => chunk,
            Err(error) => {
                print_chunk(script, vm.chunk());
                panic!("Error while compiling script: {}", error);
            }
        };

        match vm.run(chunk) {
            Ok(_) => {
                assert_eq!(output.lock().as_str(), expected_output);
            }
            Err(e) => {
                print_chunk(script, vm.chunk());
                panic!("Error while running script: {}", e.to_string());
            }
        }
    }

    #[test]
    fn print_loop() {
        let script = "
import io.print
for i in 0..5
  print 'foo {}', i
";
        check_logged_output(
            script,
            "\
foo 0
foo 1
foo 2
foo 3
foo 4
",
        );
    }

    #[test]
    fn debug() {
        let script = "debug 2 + 2";

        check_logged_output(script, "[1] 2 + 2: 4\n");
    }

    #[test]
    fn write_via_stdout() {
        let script = "
stdout = io.stdout()
stdout.write 'abc'
stdout.write 'def'
stdout.write_line 'ghi'
";

        check_logged_output(script, "abcdefghi\n");
    }

    #[test]
    fn write_via_stderr() {
        let script = "
stderr = io.stderr()
stderr.write '123'
stderr.write '456'
stderr.write_line '789'
";

        check_logged_output(script, "123456789\n");
    }
}

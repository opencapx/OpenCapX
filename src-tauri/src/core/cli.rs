//! Shared plumbing for the module-owned clap surfaces (`rules` / `guard` / `sandbox` / `automation`).

use clap::Parser;

/// Parse `args` as the `<name>` subcommand. argv[0] is synthesized so the generated usage line
/// reads naturally; on error clap has already printed its message and the exit code is returned
/// (2 = usage error, 0 = help/version).
pub fn parse<P: Parser>(name: &str, args: &[String]) -> Result<P, i32> {
    let argv = std::iter::once(name.to_string()).chain(args.iter().cloned());
    P::try_parse_from(argv).map_err(|e| {
        let code = if e.use_stderr() { 2 } else { 0 };
        let _ = e.print();
        code
    })
}

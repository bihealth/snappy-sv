use rust_htslib::{bam, bcf};

/// Global error type.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// A command line option is missing.
    #[error("missing command line argument")]
    OptionMissing(),
    /// Inconsistent input files.
    #[error("inconsistent input files")]
    InconsistentInput(),
    /// The output file already exists.
    #[error("output file already exists")]
    OutputFileExists(),
    /// Incorrect cluster setting name.
    #[error("unknown cluster setting name")]
    UnknownClusterSettingName(),
    /// Problem with file I/O.
    #[error("problem with I/O")]
    Io {
        #[from]
        source: std::io::Error,
        // TODO: add experimental backtrace feature?
    },
    /// Problem with htslib
    #[error("problem with BCF file access")]
    HtslibBcfError {
        #[from]
        source: bcf::errors::Error, // TODO: add experimental backtrace feature?
    },
    /// Problem with htslib
    #[error("problem with BAM file access")]
    HtslibBamError {
        #[from]
        source: bam::errors::Error, // TODO: add experimental backtrace feature?
    },
    /// Problem with string conversion
    #[error("problem with string conversion")]
    StrUtf8Error {
        #[from]
        source: std::str::Utf8Error, // TODO: add experimental backtrace feature?
    },
    /// Problem with string conversion
    #[error("problem with string conversion")]
    StringUtf8Error {
        #[from]
        source: std::string::FromUtf8Error, // TODO: add experimental backtrace feature?
    },
}

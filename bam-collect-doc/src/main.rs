/// bam-collect-doc -- Collect depth of coverage evidence from BAM.
use std::fs;
use std::path::Path;

use bio_types::genome::{AbstractInterval, Interval};
use clap::{App, Arg, ArgMatches};
use git_version::git_version;
use indicatif::{ProgressBar, ProgressStyle};
use itertools::sorted;
use log::{debug, info, LevelFilter};
use rust_htslib::{bam, bam::Read as BamRead, bcf, bcf::Read as BcfRead};
use separator::Separatable;
use tempfile::tempdir;

use lib_common::bam::{build_chroms_bam, samples_from_file};
use lib_common::bcf::guess_bcf_format;
use lib_common::doc::{load_doc_median, MedianReadDepthInfo};
use lib_common::error::Error;
use lib_common::parse_region;
use lib_config::Config;

mod agg;
use agg::{BamRecordAggregator, CoverageAggregator, FragmentsAggregator};
mod reference;
use reference::ReferenceStats;

/// Command line options
#[derive(Debug)]
struct Options {
    /// Verbosity level
    verbosity: u64,
    /// List of regions to call.
    regions: Option<Vec<Interval>>,
    /// Path to configuration file to use,
    path_config: Option<String>,
    /// Path to input file.
    path_input: String,
    /// Path to output file.
    path_output: String,
    /// Overwrite output file.
    overwrite: bool,
}

impl Options {
    pub fn from_arg_matches<'a>(matches: &ArgMatches<'a>) -> Result<Self, Error> {
        Ok(Options {
            verbosity: matches.occurrences_of("v"),
            regions: matches
                .value_of("regions")
                .map(|s| {
                    let x: Result<Vec<Interval>, Error> =
                        s.split(',').map(|t| parse_region(&t)).collect();
                    x
                })
                .transpose()?,
            path_config: matches.value_of("config").map(|s| s.to_string()),
            path_input: match matches.value_of("input") {
                Some(x) => String::from(x),
                None => return Err(Error::OptionMissing()),
            },
            path_output: match matches.value_of("output") {
                Some(x) => String::from(x),
                None => return Err(Error::OptionMissing()),
            },
            overwrite: matches.occurrences_of("overwrite") > 0,
        })
    }
}

/// Build header for the coverage output BCF file.
fn build_header(samples: &[String], contigs: &[Interval]) -> bcf::Header {
    let mut header = bcf::Header::new();

    // Put overall meta information into the BCF header.
    let now = chrono::Utc::now();
    if !cfg!(test) {
        header.push_record(format!("##fileDate={}", now.format("%Y%m%d").to_string()).as_bytes());
    } else {
        header.push_record(b"##fileDate=20200828");
    }

    // Add samples to BCF header.
    for sample in samples {
        header.push_sample(sample.as_bytes());
    }

    // Put contig information into BCF header.
    contigs.iter().for_each(|contig| {
        header.push_record(
            format!(
                "##contig=<ID={},length={}>",
                contig.contig(),
                contig.range().end
            )
            .as_bytes(),
        );
    });

    // Push the relevant header records.
    // TODO: later decide about commented-out lines
    let lines = vec![
        // Define ALT column <WINDOW>/<TARGET>
        "##ALT=<ID=WINDOW,Description=\"Record describes a window for read or coverage \
         counting\">",
        // INFO fields describing the window
        "##INFO=<ID=END,Number=1,Type=Integer,Description=\"Window end\">",
        "##INFO=<ID=MAPQ,Number=1,Type=Float,Description=\"Mean MAPQ value across samples \
         for approximating mapability\">",
        "##INFO=<ID=GC,Number=1,Type=Float,Description=\"Reference GC fraction, if reference \
         FASTA file was given\">",
        "##INFO=<ID=GAP,Number=0,Type=Flag,Description=\"Window overlaps with N in \
         reference (gap)\">",
        // Generic FORMAT fields
        "##FORMAT=<ID=GT,Number=1,Type=String,Description=\"Genotype\">",
        "##FORMAT=<ID=MQ,Number=1,Type=Float,Description=\"Mean read MAPQ from region\">",
        // The meaning of coverage differs between the counting approaches (fragments vs. coverage).
        "##FORMAT=<ID=RCV,Number=1,Type=Float,Description=\"Raw coverage value\">",
        "##FORMAT=<ID=RCVSD,Number=1,Type=Float,Description=\"Raw coverage standard deviation\">",
    ];
    for line in lines {
        header.push_record(line.as_bytes());
    }

    header
}

/// Build bcf::Writer with appropriate header.
fn build_bcf_writer(
    path: &str,
    samples: &[String],
    contigs: &[Interval],
) -> Result<bcf::Writer, Error> {
    let guessed = guess_bcf_format(&path);

    let header = build_header(samples, contigs);
    Ok(bcf::Writer::from_path(
        &path,
        &header,
        guessed.uncompressed,
        guessed.format,
    )?)
}

/// Process one region.
fn process_region(
    options: &Options,
    config: &Config,
    contig: &Interval,
    bcf_writer: &mut bcf::Writer,
) -> Result<(), Error> {
    info!(
        "Processing contig {}:{}-{}",
        contig.contig(),
        (contig.range().start + 1).separated_string(),
        contig.range().end.separated_string(),
    );

    let window_length = config.collect_doc_config.window_length;
    let ref_stats = config
        .path_reference_fasta
        .as_ref()
        .map(|path| ReferenceStats::from_path(path, contig.contig(), window_length))
        .transpose()?;

    let mut aggregator: Box<dyn BamRecordAggregator> =
        match config.collect_doc_config.count_kind.as_str() {
            "fragments" => Box::new(FragmentsAggregator::new(
                config.collect_doc_config.clone(),
                contig.clone(),
            )),
            "coverage" => Box::new(CoverageAggregator::new(
                config.collect_doc_config.clone(),
                contig.clone(),
            )),
            _ => panic!("Invalid combination of coverage/on-target regions"),
        };

    // TODO: 2 blocks -> function
    // Jump to region with BAM reader.
    let mut bam_reader = bam::IndexedReader::from_path(&options.path_input)?;
    if config.htslib_io_threads > 0 {
        bam_reader.set_threads(config.htslib_io_threads)?;
    }
    let tid: u32 = bam_reader.header().tid(contig.contig().as_bytes()).unwrap();
    bam_reader.fetch(tid, contig.range().start, contig.range().end)?;

    let progress_bar = if options.verbosity == 0 {
        let prog_bar =
            ProgressBar::new(((contig.range().end - contig.range().start) / 1_000) as u64);
        prog_bar.set_style(
            ProgressStyle::default_bar()
                .template(
                    "scanning {msg:.green.bold} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
            {pos:>7}/{len:7} Kbp {elapsed}/{eta}",
                )
                .progress_chars("=>-"),
        );
        prog_bar.set_message(&contig.contig());
        Some(prog_bar)
    } else {
        None
    };

    // Main loop for region: pass all BAM records in region through aggregator.
    info!("Computing coverage...");
    aggregator.put_fetched_records(&mut bam_reader, &|pos| {
        if let Some(prog_bar) = &progress_bar {
            if pos >= 0 {
                prog_bar.set_position((pos / 1_000) as u64);
            }
        }
    })?;
    debug!(
        "Processed {}, skipped {} records ({:.2}% were processed)",
        aggregator.num_processed().separated_string(),
        aggregator.num_skipped().separated_string(),
        100.0 * (aggregator.num_processed() - aggregator.num_skipped()) as f64
            / aggregator.num_processed() as f64,
    );

    // TODO: full block -> function
    // Create the BCF records for this region.
    info!("Writing BCF with coverage information...");
    for region_id in 0..aggregator.num_regions() {
        let stats = aggregator.get_stats(region_id);
        let mut record = bcf_writer.empty_record();
        let rid = bcf_writer.header().name2rid(contig.contig().as_bytes())?;

        // Columns: CHROM, POS, ID, REF, ALT, (FILTER)
        let pos = stats.start;
        let window_end = stats.end;
        let alleles_v = vec![Vec::from("N"), Vec::from("<WINDOW>")];
        let alleles = alleles_v
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<&[u8]>>();

        record.set_rid(Some(rid));
        record.set_pos(pos as i64);
        record.set_id(format!("{}:{}-{}", &contig.contig(), pos + 1, window_end).as_bytes())?;
        record.set_alleles(&alleles)?;

        // Columns: INFO
        record.push_info_integer(b"END", &[window_end as i32])?;
        if let Some(ref_stats) = ref_stats.as_ref() {
            let bucket = pos / window_length;
            let gc = ref_stats.gc_content[bucket];
            if !gc.is_nan() {
                record.push_info_float(b"GC", &[gc])?;
            }
            if ref_stats.has_gap[bucket] {
                record.push_info_flag(b"GAP")?;
            }
        }

        // Columns: FORMAT/GT
        record.push_format_integer(b"GT", &[0, 0])?;

        // Columns: FORMAT/CV etc.
        record.push_format_float(b"RCV", &[stats.cov])?;
        if let Some(cov_sd) = stats.cov_sd {
            record.push_format_float(b"RCVSD", &[cov_sd])?;
        }

        record.push_format_float(b"MQ", &[stats.mean_mapq])?;
        record.push_info_float(b"MAPQ", &[stats.mean_mapq])?;

        bcf_writer.write(&record)?;
    }

    Ok(())
}

fn perform_final_write(
    path_in: &str,
    path_out: &str,
    doc_median_info: &MedianReadDepthInfo,
) -> Result<(), Error> {
    let mut reader = bcf::Reader::from_path(&path_in)?;
    let mut header = bcf::Header::from_template(reader.header());
    let sample = std::str::from_utf8(reader.header().samples()[0])?;
    // NB: we need to prefix the underscore because htslib does not digits in front of keys
    let by_contig = sorted(
        doc_median_info
            .by_chrom
            .iter()
            .map(|(k, v)| format!("_{}={}", k, v)),
    )
    .collect::<Vec<String>>();
    header.push_record(
        format!(
            "##median-coverage=<ID={},autosomes={},{}>",
            &sample,
            doc_median_info.on_autosomes,
            by_contig.join(",")
        )
        .as_bytes(),
    );

    let guessed = guess_bcf_format(&path_out);
    let mut writer =
        bcf::Writer::from_path(&path_out, &header, guessed.uncompressed, guessed.format)?;
    let mut record = reader.empty_record();
    while reader.read(&mut record)? {
        writer.translate(&mut record);
        writer.write(&record)?;
    }

    Ok(())
}

fn perform_collection(options: &Options, config: &Config) -> Result<(), Error> {
    // Create output file writer and kick off processing.  This is done in its own block such
    // that the file is definitely closed when building the index below.
    let contigs = {
        let bam_reader = bam::IndexedReader::from_path(&options.path_input)?;
        build_chroms_bam(bam_reader.header(), None)?
    };

    let regions = if let Some(regions) = &options.regions {
        regions.clone()
    } else {
        contigs.clone()
    };

    let samples = samples_from_file(&options.path_input)?;

    // Write to temporary directory.
    info!("Scan BAM file for coverage information; write results to temporary file.");
    let tmp_dir = tempdir()?;
    let tmp_path = tmp_dir.path();
    let tmp_out = tmp_path.join("tmp.bcf").to_str().unwrap().to_string();
    {
        let mut writer = build_bcf_writer(&tmp_out, &samples, &contigs)?;
        for region in &regions {
            process_region(&options, &config, &region, &mut writer)?;
        }
    }

    info!("Done scanning BAM. Will now compute per-contig coverage medians.");
    let doc_median_info = load_doc_median(&tmp_out)?;

    info!("Done computing per-contig coverage medians. Building final coverage file.");
    perform_final_write(&tmp_out, &options.path_output, &doc_median_info)?;

    // Close temporary directory to we can handle any errors here.
    tmp_dir.close()?;

    Ok(())
}

fn main() -> Result<(), Error> {
    // Setup command line parser and parse options.
    let matches = App::new("maelstrom-bam-collect-doc")
        .version(git_version!())
        .author("Manuel Holtgrewe <manuel.holtgrewe@bihealth.de>")
        .about("Collect depth of coverage evidence from BAM")
        .args(&[
            Arg::from_usage("-v... 'Increase verbosity'"),
            Arg::from_usage("--overwrite 'Allow overwriting of output file'"),
            Arg::from_usage("-c, --config=[FILE] 'Sets a custom config file'"),
            Arg::from_usage("-r, --regions=[REGIONS] 'comma-separated list of regions'"),
            Arg::from_usage("<input> 'input file to read from'"),
            Arg::from_usage("<output> 'output file to write to'"),
        ])
        .get_matches();
    let options = Options::from_arg_matches(&matches)?;

    // Output file must not exist yet.
    if options.path_output != "-"
        && options.path_output != "/dev/stdout"
        && Path::new(&options.path_output).exists()
        && !options.overwrite
    {
        return Err(Error::OutputFileExists());
    }

    // Setup logging verbosity.
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("[%Y-%m-%d %H:%M:%S]"),
                record.level(),
                message
            ))
        })
        .level(if matches.is_present("v") {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        })
        .chain(std::io::stderr())
        .apply()
        .unwrap();
    info!("Starting maelstrom-bam-collect-doc");
    info!("options: {:?}", &options);

    // Parse further settings from configuration file.
    let config: Config = match &options.path_config {
        None => toml::from_str("").unwrap(),
        Some(path_config) => {
            debug!("Loading config file: {}", &path_config);
            let contents = fs::read_to_string(&path_config)?;
            toml::from_str(&contents).unwrap()
        }
    };
    info!("options: {:?}", &config);

    perform_collection(&options, &config)?;

    info!("All done. Have a nice day!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Interval;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempdir::TempDir;

    /// Helper that runs `perform_collection()` and compares the result.
    fn _perform_collection_and_test(
        tmp_dir: &TempDir,
        path_input: &str,
        path_expected: &str,
        count_kind: &str,
        regions: &Option<Vec<Interval>>,
    ) -> Result<(), super::Error> {
        let path_output = String::from(tmp_dir.path().join("out.vcf").to_str().unwrap());
        let options = super::Options {
            verbosity: 1, // disable progress bar
            regions: regions.clone(),
            path_config: None,
            path_input: String::from(path_input),
            path_output: path_output.clone(),
            overwrite: false,
        };
        let config: super::Config = toml::from_str(&format!(
            "[collect_doc_config]\n\
            count_kind = \"{}\"",
            count_kind
        ))
        .unwrap();

        super::perform_collection(&options, &config)?;

        assert_eq!(
            fs::read_to_string(path_expected).unwrap(),
            fs::read_to_string(&path_output).unwrap()
        );

        Ok(())
    }

    #[test]
    fn test_perform_collection_examples_fragments() -> Result<(), super::Error> {
        let tmp_dir = TempDir::new("tests")?;
        _perform_collection_and_test(
            &tmp_dir,
            "./src/tests/data/ex.sorted.bam",
            "./src/tests/data/ex.expected.fragments.vcf",
            "fragments",
            &None,
        )?;
        Ok(())
    }

    #[test]
    fn test_perform_collection_examples_coverage() -> Result<(), super::Error> {
        let tmp_dir = TempDir::new("tests")?;
        _perform_collection_and_test(
            &tmp_dir,
            "./src/tests/data/ex.sorted.bam",
            "./src/tests/data/ex.expected.coverage.vcf",
            "coverage",
            &None,
        )?;
        Ok(())
    }
}

/// lib-common -- shared functionality
use rust_htslib::{bam, bcf};

/// Return `bam::Format` for the given filename.
pub fn guess_bam_format(filename: &str) -> bam::Format {
    if filename.ends_with(".bam") {
        bam::Format::BAM
    } else {
        bam::Format::SAM
    }
}

#[derive(Debug)]
pub struct BcfFormatInfo {
    /// Guessed format.
    pub format: bcf::Format,
    /// Guessed compression status.
    pub uncompressed: bool,
}

/// Return `bcf::Format` for the given filename.
pub fn guess_bcf_format(filename: &str) -> BcfFormatInfo {
    if filename.ends_with(".bcf") {
        return BcfFormatInfo {
            format: bcf::Format::BCF,
            uncompressed: false,
        };
    } else if filename.ends_with(".vcf.gz") {
        return BcfFormatInfo {
            format: bcf::Format::VCF,
            uncompressed: false,
        };
    } else {
        return BcfFormatInfo {
            format: bcf::Format::VCF,
            uncompressed: true,
        };
    }
}

/// Build new `bcf::Header`.
pub fn build_vcf_header(template: &bcf::header::HeaderView) -> Result<bcf::Header, bcf::Error> {
    let mut header = bcf::Header::new();

    // Copy over sequence records.
    for record in template.header_records() {
        match record {
            bcf::header::HeaderRecord::Contig { key: _, values } => {
                header.push_record(
                    format!(
                        "##contig=<ID={},length={}>",
                        values
                            .get("ID")
                            .expect("source contig header does not have ID"),
                        values
                            .get("length")
                            .expect("source contig header does not have length")
                    )
                    .as_bytes(),
                );
            }
            _ => (),
        }
    }

    // Fields: ALT, INFO, FORMAT.
    let alts = vec![
        ("DEL", "Deletion"),
        ("DUP", "Duplication"),
        ("INV", "Inversion"),
    ];
    for (id, desc) in alts {
        header.push_record(format!("##ALT=<ID={},length={}>", &id, &desc).as_bytes());
    }
    let infos = vec![
        ("SVTYPE", "1", "String", "Type of structural variant"),
        ("CHR2", "1", "String", "Chromosome of end coordinate"),
        ("END", "1", "Integer", "End position of linear SV"),
        ("END2", "1", "Integer", "End position of BND"),
        ("STRANDS", "1", "String", "Breakpoint strandedness"),
        ("SVLEN", "1", "Integer", "SV length"),
        ("ALGORITHMS", ".", "String", "Source algorithms"),
    ];
    for (id, number, type_, desc) in infos {
        header.push_record(
            format!(
                "##INFO=<ID={},Number={},Type={},Description={}>",
                &id, &number, &type_, &desc
            )
            .as_bytes(),
        );
    }
    let formats = vec![
        ("GT", "1", "String", "Genotype"),
        ("delly", "1", "Integer", "Called by Delly"),
    ];
    for (id, number, type_, desc) in formats {
        header.push_record(
            format!(
                "##FORMAT=<ID={},Number={},Type={},Description={}>",
                &id, &number, &type_, &desc
            )
            .as_bytes(),
        );
    }

    // Add samples.
    for name in template.samples() {
        header.push_sample(name);
    }

    Ok(header)
}

/// Enumeration for calling algorithms.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Algorithm {
    /// Delly2
    Delly,
}

#[cfg(test)]
mod tests {
    use super::*;
    use matches::assert_matches;

    #[test]
    fn test_guess_bam_format() {
        assert_matches!(guess_bam_format("ex.sam"), bam::Format::SAM);
        assert_matches!(guess_bam_format("ex.bam"), bam::Format::BAM);
        assert_matches!(guess_bam_format("ex.xxx"), bam::Format::SAM);
    }

    #[test]
    fn test_guess_bcf_format() {
        assert_matches!(guess_bcf_format("ex.vcf").format, bcf::Format::VCF);
        assert_eq!(guess_bcf_format("ex.vcf").uncompressed, true);
        assert_matches!(guess_bcf_format("ex.vcf.gz").format, bcf::Format::VCF);
        assert_eq!(guess_bcf_format("ex.vcf.gz").uncompressed, false);
        assert_matches!(guess_bcf_format("ex.bcf").format, bcf::Format::BCF);
        assert_eq!(guess_bcf_format("ex.bcf").uncompressed, false);
        assert_matches!(guess_bcf_format("ex.xxx").format, bcf::Format::VCF);
        assert_eq!(guess_bcf_format("ex.xxx").uncompressed, true);
    }
}
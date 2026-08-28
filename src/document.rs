use std::{
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use calamine::{open_workbook_auto, Reader};
use quick_xml::{events::Event, Reader as XmlReader};
use thiserror::Error;
use zip::ZipArchive;

const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EXTRACTED_CHARS: usize = 12_000;
const MAX_XML_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ReferenceDocument {
    pub name: String,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("参考文档不存在：{0}")]
    NotFound(String),
    #[error("参考文档必须是普通文件：{0}")]
    NotAFile(String),
    #[error("暂只支持 .txt、.docx 和 .xlsx 参考文档")]
    UnsupportedFormat,
    #[error("参考文档不能超过 20MB")]
    FileTooLarge,
    #[error("参考文档没有可提取的文字")]
    Empty,
    #[error("无法读取参考文档：{0}")]
    Read(String),
    #[error("无法解析参考文档：{0}")]
    Parse(String),
}

pub fn validate_reference_path(raw_path: &str) -> Result<Option<PathBuf>, DocumentError> {
    let trimmed = raw_path.trim().trim_matches(['"', '\'']);
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = expand_home(trimmed);
    let metadata =
        fs::metadata(&path).map_err(|_| DocumentError::NotFound(path.display().to_string()))?;
    if !metadata.is_file() {
        return Err(DocumentError::NotAFile(path.display().to_string()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(DocumentError::FileTooLarge);
    }
    match extension(&path).as_deref() {
        Some("txt" | "docx" | "xlsx") => Ok(Some(path)),
        _ => Err(DocumentError::UnsupportedFormat),
    }
}

pub fn load_reference_document(raw_path: &str) -> Result<Option<ReferenceDocument>, DocumentError> {
    let Some(path) = validate_reference_path(raw_path)? else {
        return Ok(None);
    };
    let extracted = match extension(&path).as_deref() {
        Some("txt") => read_text(&path)?,
        Some("docx") => read_docx(&path)?,
        Some("xlsx") => read_xlsx(&path)?,
        _ => return Err(DocumentError::UnsupportedFormat),
    };
    let normalized = normalize_whitespace(&extracted);
    if normalized.is_empty() {
        return Err(DocumentError::Empty);
    }
    let (content, truncated) = truncate_chars(normalized, MAX_EXTRACTED_CHARS);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("参考文档")
        .to_owned();
    Ok(Some(ReferenceDocument {
        name,
        content,
        truncated,
    }))
}

fn expand_home(path: &str) -> PathBuf {
    let Some(relative) = path.strip_prefix("~/") else {
        return PathBuf::from(path);
    };
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home_directory| home_directory.join(relative))
        .unwrap_or_else(|| PathBuf::from(path))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

fn read_text(path: &Path) -> Result<String, DocumentError> {
    fs::read_to_string(path).map_err(|error| DocumentError::Read(error.to_string()))
}

fn read_docx(path: &Path) -> Result<String, DocumentError> {
    let file = File::open(path).map_err(|error| DocumentError::Read(error.to_string()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| DocumentError::Parse(format!("DOCX ZIP：{error}")))?;
    let document = archive
        .by_name("word/document.xml")
        .map_err(|error| DocumentError::Parse(format!("缺少 word/document.xml：{error}")))?;
    if document.size() > MAX_XML_BYTES {
        return Err(DocumentError::FileTooLarge);
    }
    let mut xml = String::new();
    document
        .take(MAX_XML_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(|error| DocumentError::Read(error.to_string()))?;
    extract_docx_text(&xml)
}

fn extract_docx_text(xml: &str) -> Result<String, DocumentError> {
    let mut reader = XmlReader::from_str(xml);
    let mut output = String::new();
    let mut inside_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                inside_text = event.local_name().as_ref() == "t";
                if event.local_name().as_ref() == "tab" {
                    output.push('\t');
                }
            }
            Ok(Event::Empty(event)) if event.local_name().as_ref() == "tab" => {
                output.push('\t');
            }
            Ok(Event::Empty(event)) if event.local_name().as_ref() == "br" => {
                output.push('\n');
            }
            Ok(Event::Text(text)) if inside_text => {
                output.push_str(&text.xml10_content());
            }
            Ok(Event::GeneralRef(reference)) if inside_text => {
                if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|error| DocumentError::Parse(error.to_string()))?
                {
                    output.push(character);
                } else if let Some(entity) = match reference.as_ref() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                } {
                    output.push(entity);
                }
            }
            Ok(Event::End(event)) => {
                if event.local_name().as_ref() == "t" {
                    inside_text = false;
                } else if event.local_name().as_ref() == "p" {
                    output.push('\n');
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(DocumentError::Parse(error.to_string())),
            _ => {}
        }
    }
    Ok(output)
}

fn read_xlsx(path: &Path) -> Result<String, DocumentError> {
    let mut workbook =
        open_workbook_auto(path).map_err(|error| DocumentError::Parse(format!("XLSX：{error}")))?;
    let sheet_names = workbook.sheet_names().to_owned();
    let mut output = String::new();
    for sheet_name in sheet_names {
        if output.chars().count() >= MAX_EXTRACTED_CHARS {
            break;
        }
        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(|error| DocumentError::Parse(format!("工作表 {sheet_name}：{error}")))?;
        output.push_str("[工作表：");
        output.push_str(&sheet_name);
        output.push_str("]\n");
        for row in range.rows() {
            let values = row
                .iter()
                .map(ToString::to_string)
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                output.push_str(&values.join("\t"));
                output.push('\n');
            }
            if output.chars().count() >= MAX_EXTRACTED_CHARS {
                break;
            }
        }
    }
    Ok(output)
}

fn normalize_whitespace(input: &str) -> String {
    input
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(input: String, limit: usize) -> (String, bool) {
    if input.chars().count() <= limit {
        return (input, false);
    }
    let mut truncated = input.chars().take(limit).collect::<String>();
    truncated.push_str("\n[内容已截断]");
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;
    use zip::{write::SimpleFileOptions, ZipWriter};

    use super::*;

    fn add_zip_entry(archive: &mut ZipWriter<File>, name: &str, content: &str) {
        archive
            .start_file(name, SimpleFileOptions::default())
            .expect("start ZIP entry");
        archive
            .write_all(content.as_bytes())
            .expect("write ZIP entry");
    }

    #[test]
    fn loads_and_normalizes_utf8_text() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("brief.txt");
        fs::write(&path, "产品名   Aurora\n\n 固定译法  Dawn").expect("write text reference");

        let document = load_reference_document(path.to_str().expect("UTF-8 path"))
            .expect("load reference")
            .expect("document");
        assert_eq!(document.name, "brief.txt");
        assert_eq!(document.content, "产品名 Aurora\n固定译法 Dawn");
        assert!(!document.truncated);
    }

    #[test]
    fn extracts_paragraphs_from_docx_xml() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("brief.docx");
        let file = File::create(&path).expect("create docx");
        let mut archive = ZipWriter::new(file);
        archive
            .start_file("word/document.xml", SimpleFileOptions::default())
            .expect("start document XML");
        archive
            .write_all(
                r#"<w:document xmlns:w="urn:test"><w:body><w:p><w:r><w:t>Aurora &amp; Dawn</w:t></w:r></w:p><w:p><w:r><w:t>第二段</w:t></w:r></w:p></w:body></w:document>"#
                    .as_bytes(),
            )
            .expect("write document XML");
        archive.finish().expect("finish docx");

        let document = load_reference_document(path.to_str().expect("UTF-8 path"))
            .expect("load docx")
            .expect("document");
        assert_eq!(document.content, "Aurora & Dawn\n第二段");
    }

    #[test]
    fn extracts_cells_and_sheet_name_from_xlsx() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("brief.xlsx");
        let file = File::create(&path).expect("create xlsx");
        let mut archive = ZipWriter::new(file);
        add_zip_entry(
            &mut archive,
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        );
        add_zip_entry(
            &mut archive,
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        );
        add_zip_entry(
            &mut archive,
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Terms" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        );
        add_zip_entry(
            &mut archive,
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        );
        add_zip_entry(
            &mut archive,
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>产品名</t></is></c><c r="B1" t="inlineStr"><is><t>Aurora</t></is></c></row></sheetData></worksheet>"#,
        );
        archive.finish().expect("finish xlsx");

        let document = load_reference_document(path.to_str().expect("UTF-8 path"))
            .expect("load xlsx")
            .expect("document");
        assert_eq!(document.content, "[工作表：Terms]\n产品名 Aurora");
    }

    #[test]
    fn rejects_unsupported_documents() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("brief.pdf");
        fs::write(&path, "not supported").expect("write unsupported file");
        assert!(matches!(
            load_reference_document(path.to_str().expect("UTF-8 path")),
            Err(DocumentError::UnsupportedFormat)
        ));
    }
}

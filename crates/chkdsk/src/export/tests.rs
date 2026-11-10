use super::*;
use crate::{
    collector::{DiskSnapshot, Reading},
    fixtures::{self, NOW},
};
use chkdsk_core::model::DiskKind;
use serde_json::Value;

fn json_of(snapshots: &[DiskSnapshot], history: &History) -> Value {
    let json = to_json(snapshots, history, NOW).expect("the export serializes");
    serde_json::from_str(&json).expect("the export is valid JSON")
}

/// The fixture history of the first disk, as the collector would have stored it.
fn fixture_history() -> History {
    let disks = fixtures::disks();
    fixtures::history(&disks[0].identity, &[(33, 5.0), (1, 7.0)])
}

fn rows(csv: &str) -> Vec<&str> {
    csv.trim_start_matches('\u{feff}')
        .split("\r\n")
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn json_lists_every_disk_with_identity_telemetry_attributes_and_history() {
    let disks = fixtures::disks();
    let document = json_of(&disks, &fixture_history());
    assert_eq!(document["app"], "chkdsk");
    assert_eq!(document["exported_at"], "2027-01-15T08:00:00Z");
    let listed = document["disks"].as_array().expect("a disk list");
    assert_eq!(listed.len(), disks.len(), "every disk, healthy or not");

    let nvme = &listed[0];
    assert_eq!(nvme["model"], "Samsung SSD 990 PRO 1TB");
    assert_eq!(nvme["serial"], "S73VNJ0W412345");
    assert_eq!(nvme["firmware"], "4B2QJXD7");
    assert_eq!(nvme["bus"], "NVMe");
    assert_eq!(nvme["kind"], "nvme");
    assert_eq!(nvme["capacity_bytes"], 1_000_204_886_016u64);
    assert_eq!(nvme["status"], "Saudável");
    assert_eq!(nvme["tone"], "healthy");
    assert_eq!(nvme["score"], 93);
    assert_eq!(nvme["telemetry"]["temperature_c"], 38);
    assert_eq!(nvme["telemetry"]["power_on_hours"], 4_380);
    assert_eq!(nvme["telemetry"]["wear_percent"], 7.0);
    assert_eq!(nvme["telemetry"]["nvme_health_log"]["percentage_used"], 7);
    let history = nvme["history"].as_array().expect("history samples");
    assert_eq!(history.len(), 2, "the stored samples of this disk");
    assert_eq!(history[1]["endurance_percent"], 7.0);
    assert!(listed[1]["history"].as_array().unwrap().is_empty());

    let hdd = &listed[1];
    assert_eq!(hdd["kind"], "hdd");
    assert_eq!(hdd["tone"], "attention");
    assert_eq!(hdd["verdict"], "O dispositivo relatou uma falha");
    assert!(
        hdd["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == "Setores pendentes aguardando remapeamento")
    );
    let attributes = hdd["smart_attributes"].as_array().unwrap();
    assert_eq!(attributes.len(), 15);
    let reallocated = attributes.iter().find(|a| a["id"] == 5).expect("ID 5");
    assert_eq!(reallocated["name"], "Setores realocados");
    assert_eq!(reallocated["current"], 41);
    assert_eq!(reallocated["worst"], 41);
    assert_eq!(reallocated["threshold"], 36);
    assert_eq!(reallocated["raw"], "0000000006C8");
    let vendor = attributes.iter().find(|a| a["id"] == 0xC2).unwrap();
    assert!(
        vendor["name"].is_null(),
        "a vendor attribute has no shared name"
    );
    assert!(
        vendor["threshold"].is_null(),
        "no declared threshold stays null"
    );

    // A disk with no reading is still listed, saying why it has no numbers.
    assert_eq!(listed[3]["status"], "Lendo…");
    assert!(listed[3]["score"].is_null());
    assert!(
        listed[4]["verdict"]
            .as_str()
            .unwrap()
            .contains("Uma nova tentativa")
    );
}

#[test]
fn csv_has_a_byte_order_mark_a_header_and_one_row_per_disk() {
    let disks = fixtures::disks();
    let csv = to_csv(&disks, NOW);
    assert!(csv.starts_with('\u{feff}'), "UTF-8 BOM for Excel");
    assert!(csv.ends_with("\r\n"), "CRLF rows");
    assert!(!csv.contains(';'), "comma separated, never a semicolon");
    let lines = rows(&csv);
    assert_eq!(
        lines[0],
        "model,kind,capacity_bytes,score_percent,status,wear_percent,temperature_c,power_on_hours,last_reading_utc"
    );
    assert_eq!(lines.len(), 1 + disks.len(), "one row per disk");
    assert_eq!(
        lines[1],
        "Samsung SSD 990 PRO 1TB,nvme,1000204886016,93,Saudável,7,38,4380,2027-01-13T20:00:00Z"
    );
    // The failing hard drive has hours and a temperature but no wear figure.
    assert_eq!(
        lines[2],
        "WDC WD10EZEX-08WN4A0,hdd,1000204886016,20,Crítico,,34,33815,"
    );
    // A disk still being read has only what discovery knew.
    assert_eq!(lines[4], "Kingston NV2 500GB,nvme,500107862016,,Lendo…,,,,");
}

#[test]
fn csv_quotes_fields_as_rfc_4180_and_defuses_formulas() {
    assert_eq!(quote("plain"), "plain");
    assert_eq!(quote("a,b"), "\"a,b\"");
    assert_eq!(quote("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(quote("two\r\nlines"), "\"two\r\nlines\"");

    let mut disk = fixtures::disks().remove(3);
    disk.identity.model = Some("Disk, \"Pro\" 1TB".into());
    let csv = to_csv(std::slice::from_ref(&disk), NOW);
    assert_eq!(
        rows(&csv)[1],
        "\"Disk, \"\"Pro\"\" 1TB\",nvme,500107862016,,Lendo…,,,,"
    );

    // A model name is device input: it must not run as a spreadsheet formula.
    for risky in ["=1+1", "+SUM(A1)", "-2", "@cmd"] {
        disk.identity.model = Some(risky.into());
        let csv = to_csv(std::slice::from_ref(&disk), NOW);
        assert!(
            rows(&csv)[1].starts_with(&format!("'{risky},")),
            "{risky} is guarded: {csv}"
        );
    }
}

#[test]
fn an_empty_list_still_exports_a_valid_file() {
    let document = json_of(&[], &History::default());
    assert!(document["disks"].as_array().unwrap().is_empty());
    assert_eq!(rows(&to_csv(&[], NOW)).len(), 1, "the header alone");
}

#[test]
fn dates_are_iso_8601_utc() {
    assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        iso8601_utc(951_782_400),
        "2000-02-29T00:00:00Z",
        "a leap day"
    );
    assert_eq!(iso8601_utc(NOW), "2027-01-15T08:00:00Z");
    assert_eq!(iso8601_utc(4_102_444_799), "2099-12-31T23:59:59Z");
}

#[test]
fn the_file_name_decides_the_format_and_the_filter_fills_a_missing_extension() {
    assert_eq!(Format::of_path(Path::new("a.csv")), Format::Csv);
    assert_eq!(Format::of_path(Path::new("a.CSV")), Format::Csv);
    assert_eq!(Format::of_path(Path::new("a.json")), Format::Json);
    assert_eq!(Format::of_path(Path::new("a.txt")), Format::Json);
    assert_eq!(
        with_extension("dir/discos".into(), Format::Csv),
        PathBuf::from("dir/discos.csv")
    );
    assert_eq!(
        with_extension("dir/discos".into(), Format::Json),
        PathBuf::from("dir/discos.json")
    );
    assert_eq!(
        with_extension("dir/discos.json".into(), Format::Csv),
        PathBuf::from("dir/discos.json"),
        "a typed extension wins over the filter"
    );
}

fn target(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("chkdsk-export-{}-{name}", std::process::id()))
}

fn pick_json(_: usize, name: &str) -> Option<Pick> {
    assert_eq!(name, SUGGESTED_NAME);
    Some(Pick {
        path: target("json"),
        filter: Format::Json,
    })
}

fn pick_csv(_: usize, _: &str) -> Option<Pick> {
    Some(Pick {
        path: target("table.csv"),
        filter: Format::Json,
    })
}

fn pick_nothing(_: usize, _: &str) -> Option<Pick> {
    None
}

fn pick_missing_folder(_: usize, _: &str) -> Option<Pick> {
    Some(Pick {
        path: target("no-such-folder/inside/discos.json"),
        filter: Format::Json,
    })
}

#[test]
fn running_an_export_writes_the_chosen_format_and_reports_the_outcome() {
    let disks = fixtures::disks();

    let outcome = run(&disks, NOW, 0, Some(pick_json), fixture_history);
    let path = target("json.json");
    assert_eq!(
        outcome,
        Some(Outcome::Saved {
            name: path.file_name().unwrap().to_string_lossy().into()
        }),
        "the filter added .json"
    );
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["disks"].as_array().unwrap().len(), disks.len());
    assert_eq!(written["disks"][0]["history"].as_array().unwrap().len(), 2);
    std::fs::remove_file(&path).unwrap();

    let outcome = run(&disks, NOW, 0, Some(pick_csv), || {
        panic!("a CSV has no history to load")
    });
    let path = target("table.csv");
    assert!(matches!(outcome, Some(Outcome::Saved { .. })));
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM bytes on disk");
    assert_eq!(
        rows(&String::from_utf8(bytes).unwrap()).len(),
        1 + disks.len()
    );
    std::fs::remove_file(&path).unwrap();

    assert_eq!(
        run(&disks, NOW, 0, Some(pick_nothing), History::default),
        None,
        "a closed dialog has no outcome to report"
    );
    assert_eq!(
        run(&disks, NOW, 0, Some(pick_missing_folder), History::default),
        Some(Outcome::Failed(Failure::Write))
    );
    assert_eq!(
        run(&disks, NOW, 0, None, History::default),
        Some(Outcome::Unavailable)
    );
}

#[test]
fn a_file_is_replaced_in_one_piece_and_a_failure_leaves_the_old_one_alone() {
    let path = target("replace.json");
    std::fs::write(&path, b"old contents").unwrap();
    write_atomically(&path, b"new contents").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
    std::fs::remove_file(&path).unwrap();

    // A folder where the file should go: the move fails, the folder stays, and no
    // temporary file is left beside it.
    let blocked = target("blocked.json");
    std::fs::create_dir(&blocked).unwrap();
    assert!(write_atomically(&blocked, b"never lands").is_err());
    assert!(blocked.is_dir(), "the old target is untouched");
    let stem = blocked.file_name().unwrap().to_string_lossy().into_owned();
    let leftovers: Vec<_> = std::fs::read_dir(blocked.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(&stem) && name.ends_with(".tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    std::fs::remove_dir(&blocked).unwrap();
}

fn pick_and_panic(_: usize, _: &str) -> Option<Pick> {
    panic!("the dialog blew up")
}

#[test]
fn a_panic_inside_the_export_is_reported_as_a_failure() {
    let disks = fixtures::disks();
    assert_eq!(
        run_isolated(&disks, NOW, 0, Some(pick_and_panic), History::default),
        Some(Outcome::Failed(Failure::Interrupted))
    );
    // Without a panic it answers like `run`.
    assert_eq!(
        run_isolated(&disks, NOW, 0, Some(pick_nothing), History::default),
        None
    );
    assert_eq!(
        run_isolated(&disks, NOW, 0, None, History::default),
        Some(Outcome::Unavailable)
    );
}

#[test]
fn a_disk_is_exported_with_the_kind_its_reading_proved() {
    let mut disks = fixtures::disks();
    // Discovery could not tell the kind; the NVMe log did.
    disks[0].identity.kind = DiskKind::Unknown;
    let document = json_of(&disks, &History::default());
    assert_eq!(document["disks"][0]["kind"], "nvme");
    assert!(matches!(disks[3].reading, Reading::Pending));
}

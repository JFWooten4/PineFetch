//! Deterministic, local term counts over every saved text source for one video.
use crate::database::Database;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const MAX_TEXT_BYTES: usize = 128 * 1024 * 1024;
const MAX_TERMS: usize = 50_000;
static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "aber alle allem allen aller alles als also am an andere anderen anderer anderes auch auf aus bei bin bis bist da dabei dadurch dafür daher damit dann das dass daß dein deine dem den denn der des die dies diese diesem diesen dieser dieses doch dort du durch ein eine einem einen einer eines er es etwas euch euer eure für ganz gegen gehabt haben hat hatte hatten hast hier hin hinter ich ihr ihre ihrem ihren ihrer ihres im in ins ist ja jede jedem jeden jeder jedes jetzt kann kein keine keinem keinen keiner keines können konnte machen man mehr mein meine mich mir mit muss nach nicht nichts noch nun nur ob oder ohne sehr sein seine seinem seinen seiner seines selbst sich sie sind so solche sollte sondern sonst über um und uns unser unsere unter vom von vor war waren warst was welche welchem welchen welcher welches wenn werde werden werdet weshalb wie wieder wir wird wirst wo zu zum zur zwischen the a an and are as at be been being but by can could did do does for from had has have he her here hers him his how if in into is it its just may me more most my no nor not of on once only or other our out over own same she should some such than that their theirs them then there these they this those through to too under until up very was we were what when where which while who whom why will with would you your yours about also because both each few much must now off ours shall since still themselves upon us want well were all any don dont doesnt isnt wasnt wouldnt couldve ive youre thats its let lets webvtt kind language captions www com http https".split_whitespace().collect()
});

#[derive(Debug, Serialize)]
pub(crate) struct KeywordTerm {
    pub(crate) term: String,
    pub(crate) transcript: u64,
    pub(crate) ocr: u64,
    pub(crate) captions: u64,
    pub(crate) total: u64,
    pub(crate) sources: u8,
}
#[derive(Debug, Default, Serialize)]
pub(crate) struct KeywordSources {
    pub(crate) transcript: bool,
    pub(crate) ocr_frames: u64,
    pub(crate) ocr_legacy: bool,
    pub(crate) captions: u64,
}
#[derive(Debug, Serialize)]
pub(crate) struct HistoryKeywords {
    pub(crate) history_entry_id: String,
    pub(crate) sources: KeywordSources,
    pub(crate) terms: Vec<KeywordTerm>,
}

#[derive(Default)]
struct Miner {
    terms: HashMap<String, KeywordTerm>,
    bytes: usize,
}
impl Miner {
    fn add(&mut self, text: &str, source: usize) -> Result<(), String> {
        self.bytes = self.bytes.saturating_add(text.len());
        if self.bytes > MAX_TEXT_BYTES {
            return Err("The saved texts exceed the 128 MiB keyword mining limit.".into());
        }
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            let length = word.chars().count();
            if !(2..=64).contains(&length) || !word.chars().any(char::is_alphabetic) {
                continue;
            }
            let key = word.to_lowercase();
            if STOP_WORDS.contains(key.as_str()) {
                continue;
            }
            if !self.terms.contains_key(&key) && self.terms.len() >= MAX_TERMS {
                return Err("The saved texts exceed the 50,000 distinct keyword limit.".into());
            }
            let term = self.terms.entry(key).or_insert_with(|| KeywordTerm {
                term: word.into(),
                transcript: 0,
                ocr: 0,
                captions: 0,
                total: 0,
                sources: 0,
            });
            match source {
                0 => term.transcript += 1,
                1 => term.ocr += 1,
                _ => term.captions += 1,
            }
            term.total += 1;
            term.sources = u8::from(term.transcript > 0)
                + u8::from(term.ocr > 0)
                + u8::from(term.captions > 0);
        }
        Ok(())
    }
    fn finish(self) -> Vec<KeywordTerm> {
        let mut terms: Vec<_> = self
            .terms
            .into_iter()
            .filter(|(_, term)| term.total > 1)
            .collect();
        terms.sort_by(|(ak, a), (bk, b)| {
            b.sources
                .cmp(&a.sources)
                .then(b.total.cmp(&a.total))
                .then(ak.cmp(bk))
        });
        terms.into_iter().map(|(_, term)| term).collect()
    }
}

pub(crate) fn mine(db: &Database, id: &str) -> Result<HistoryKeywords, String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_entries WHERE id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("This history entry no longer exists.".into());
    }
    let mut miner = Miner::default();
    let mut sources = KeywordSources::default();
    // Each source is streamed from one read snapshot, including OCR beyond the
    // UI's first 100 frames. No source files or online services are opened.
    for (source, sql) in [
        (0, "SELECT text FROM transcriptions WHERE history_entry_id=?1"),
        (1, "SELECT text FROM history_ocr WHERE history_entry_id=?1 AND text IS NOT NULL"),
        (1, "SELECT full_text FROM history_ocr_frames WHERE history_entry_id=?1 ORDER BY timestamp_ms,id"),
        (2, "SELECT text FROM captions WHERE history_entry_id=?1 ORDER BY created_at,media_path"),
    ] {
        let mut stmt = tx.prepare(sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let text = row.get_ref(0).map_err(|e| e.to_string())?.as_str().map_err(|e| e.to_string())?;
            if text.trim().is_empty() { continue; }
            match source {
                0 => sources.transcript = true,
                1 if sql.contains("full_text") => sources.ocr_frames += 1,
                1 => sources.ocr_legacy = true,
                _ => sources.captions += 1,
            }
            miner.add(text, source)?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(HistoryKeywords {
        history_entry_id: id.into(),
        sources,
        terms: miner.finish(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    #[test]
    fn terms_match_across_sources_preserve_names_and_exclude_stop_words_numbers_and_timestamps() {
        let mut miner = Miner::default();
        miner
            .add(
                "[00:00:10 → 00:00:15] Der Nintendo DSi: Nintendo, Pokémon und AI AI in 4K 4K!",
                0,
            )
            .unwrap();
        miner.add("NINTENDO\nDSi Pokémon", 1).unwrap();
        miner.add("Nintendo Pokémon Pokémon.", 2).unwrap();
        let terms = miner.finish();
        let term = terms.iter().find(|t| t.term == "Nintendo").unwrap();
        assert_eq!(
            (
                term.transcript,
                term.ocr,
                term.captions,
                term.total,
                term.sources
            ),
            (2, 1, 1, 4, 3)
        );
        assert!(terms.iter().any(|t| t.term == "AI"));
        assert!(terms.iter().any(|t| t.term == "4K"));
        assert!(!terms
            .iter()
            .any(|t| ["Der", "und", "in", "00", "10", "15"].contains(&t.term.as_str())));
        assert_eq!(terms[0].sources, 3);
        let mut different = Miner::default();
        different
            .add("Game Game games games Spiel Spiel Spiele Spiele", 0)
            .unwrap();
        assert_eq!(different.finish().len(), 4);
    }
    #[test]
    fn german_and_english_articles_are_never_counted_in_any_source() {
        let mut miner = Miner::default();
        for source in 0..3 {
            miner
                .add(
                    "DER die Das DEM den DES EIN eine Einem EINEN einer EINES THE a AN Nintendo",
                    source,
                )
                .unwrap();
        }
        // Repetition across all three sources must not let articles qualify.
        assert_eq!(miner.terms.len(), 1);
        let terms = miner.finish();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].term, "Nintendo");
        assert_eq!(
            (terms[0].transcript, terms[0].ocr, terms[0].captions),
            (1, 1, 1)
        );
    }

    #[test]
    fn only_terms_with_two_or_more_occurrences_across_all_sources_are_returned() {
        let mut miner = Miner::default();
        miner.add("Einmal Doppelt Doppelt Gemeinsam", 0).unwrap();
        miner.add("gemeinsam Einzigartig", 1).unwrap();
        miner.add("Nurcaption", 2).unwrap();
        let terms = miner.finish();
        assert_eq!(terms.len(), 2);
        let shared = terms.iter().find(|t| t.term == "Gemeinsam").unwrap();
        assert_eq!((shared.transcript, shared.ocr, shared.total), (1, 1, 2));
        let repeated = terms.iter().find(|t| t.term == "Doppelt").unwrap();
        assert_eq!(
            (repeated.transcript, repeated.sources, repeated.total),
            (2, 1, 2)
        );
    }

    #[test]
    fn screenshot_counts_match_the_shared_text_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../test/fixtures/screenshots/keywords.json"
        ))
        .unwrap();
        let mut miner = Miner::default();
        miner
            .add(fixture["transcript"].as_str().unwrap(), 0)
            .unwrap();
        for (source, name) in [(1, "ocr"), (2, "captions")] {
            for text in fixture[name].as_array().unwrap() {
                miner.add(text.as_str().unwrap(), source).unwrap();
            }
        }
        assert_eq!(
            serde_json::to_value(miner.finish()).unwrap(),
            fixture["result"]["terms"]
        );
    }
    #[test]
    fn all_saved_frames_and_caption_sources_are_counted_without_writes_or_file_access() {
        let conn = Connection::open_in_memory().unwrap();
        crate::database::initialize(&conn).unwrap();
        conn.execute_batch("INSERT INTO history_entries(id,url,created_at) VALUES ('video','https://example.com',1),('empty','https://example.com/empty',1);
            INSERT INTO transcriptions(history_entry_id,text,type) VALUES ('video','Nintendo Nintendo language','text');
            INSERT INTO history_ocr(history_entry_id,text,last_attempt_id,last_attempt_status,last_attempt_at,result_revision) VALUES ('video','Legacy Nintendo','original','completed',1,'original-revision');
            INSERT INTO captions(history_entry_id,media_path,caption_path,text,created_at) VALUES ('video','missing.mp4','missing.txt','Nintendo captions',1),('video','other.mp4','other.txt','Nintendo second',2);").unwrap();
        for index in 0..251 {
            conn.execute("INSERT INTO history_ocr_frames(id,history_entry_id,timestamp_ms,full_text,blocks_json) VALUES (?1,'video',?2,?3,'[]')", params![format!("frame-{index}"),index*1000, if index==250 {"RareLastFrame RareLastFrame Nintendo"} else {"Nintendo"}]).unwrap();
        }
        let db = Database::new(conn);
        let result = mine(&db, "video").unwrap();
        let term = result.terms.iter().find(|t| t.term == "Nintendo").unwrap();
        assert_eq!((term.transcript, term.ocr, term.captions), (2, 252, 2));
        assert_eq!(result.sources.ocr_frames, 251);
        assert_eq!(result.sources.captions, 2);
        assert!(result.sources.ocr_legacy);
        assert!(result.terms.iter().any(|t| t.term == "RareLastFrame"));
        assert!(mine(&db, "empty").unwrap().terms.is_empty());
        assert!(mine(&db, "missing").is_err());
        let revision: String = db
            .lock()
            .unwrap()
            .query_row("SELECT result_revision FROM history_ocr", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revision, "original-revision");
        db.lock()
            .unwrap()
            .execute("DELETE FROM history_ocr_frames WHERE id='frame-250'", [])
            .unwrap();
        assert!(!mine(&db, "video")
            .unwrap()
            .terms
            .iter()
            .any(|t| t.term == "RareLastFrame"));
    }
}

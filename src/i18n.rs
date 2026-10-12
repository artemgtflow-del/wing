//! Message catalogue.
//!
//! User facing strings go through [`tr`] so they can be translated. Each entry
//! is `(key, en-US, ru-RU)`; the locale picks the column and `en-US` is the
//! fallback, so a missing translation degrades to English instead of printing
//! a key.
//!
//! Machine readable output (`--json`) is deliberately *not* routed through
//! here: those keys are a contract, not prose.

use std::sync::OnceLock;

pub const DEFAULT_LOCALE: &str = "en-US";

/// Locales that have translations. `en-US` must stay first: it is the fallback.
pub const LOCALES: &[&str] = &["en-US", "ru-RU"];

/// `(key, en-US, ru-RU)`
type Entry = (&'static str, &'static str, &'static str);

fn entries() -> &'static [Entry] {
    static E: OnceLock<Vec<Entry>> = OnceLock::new();
    E.get_or_init(|| {
        vec![
            // --- generic ---
            ("error", "error", "ошибка"),
            ("warning", "warning", "предупреждение"),
            ("note", "note", "заметка"),
            ("done", "done", "готово"),
            ("cancelled", "cancelled", "отменено"),
            // --- search / show ---
            ("no_results", "no packages matched", "ничего не найдено"),
            (
                "not_in_index",
                "`{0}` is not in the index",
                "`{0}` отсутствует в индексе",
            ),
            ("try_search", "try `wing search {0}`", "попробуйте `wing search {0}`"),
            (
                "empty_query",
                "provide a query, or one of --tag/--command/--pfn/--product-code",
                "укажите запрос или один из --tag/--command/--pfn/--product-code",
            ),
            (
                "versions_header",
                "{0} version(s), newest first, showing 6",
                "версий: {0} (сначала новые, показано 6)",
            ),
            (
                "no_manifest_versions",
                "could not load a manifest for {0}",
                "не удалось загрузить манифест для {0}",
            ),
            // --- install ---
            ("installing", "installing {0} {1}...", "установка {0} {1}..."),
            (
                "plan_header",
                "plan ({0} package(s)):",
                "план ({0} пакет(ов)):",
            ),
            ("plan_line", "{0} {1}  [{2} / {3} / {4}]  {5} MB", "{0} {1}  [{2} / {3} / {4}]  {5} МБ"),
            // --- uninstall ---
            ("uninstalling", "uninstalling {0} {1}...", "удаление {0} {1}..."),
            ("remove_prompt", "remove {0}? [y/N] ", "удалить {0}? [y/N] "),
            ("removed_links", "removed {0} command link(s)", "удалено ссылок на команды: {0}"),
            ("not_installed", "`{0}` is not installed", "`{0}` не установлен"),
            (
                "still_in_arp",
                "`{0}` still appears in Add/Remove Programs; remove it manually",
                "`{0}` всё ещё числится в «Программы и компоненты»; удалите вручную",
            ),
            (
                "msi_not_installed",
                "msiexec reports product {{{0}}} as not installed.\n\
                 The ProductCode in the manifest does not match an installed product; \
                 nothing was changed.",
                "msiexec сообщает, что продукт {{{0}}} не установлен.\n\
                 ProductCode в манифесте не совпадает с установленным пакетом; \
                 ничего не изменено.",
            ),
            (
                "no_uninstall_mechanism",
                "no uninstall mechanism found for {0} ({1})",
                "для {0} не найден способ удаления ({1})",
            ),
            // --- upgrade ---
            ("up_to_date", "everything is up to date", "всё актуально"),
            (
                "upgradable_hint",
                "\nrun `wing upgrade --all` to update everything",
                "\nвыполните `wing upgrade --all`, чтобы обновлить всё",
            ),
            ("upgraded", "upgraded {0}", "{0} обновлён"),
            // --- link ---
            ("linked", "  linked {0} -> {1}", "  созданы ссылки: {0} -> {1}"),
            (
                "link_not_found",
                "could not find the installed executable for {0}\nsearched {1} and the usual per-user and per-machine locations",
                "не удалось найти установленный исполняемый файл для {0}\nпроверены {1} и обычные пользовательские и системные каталоги",
            ),
            (
                "added_to_path",
                "  added {0} to your PATH",
                "  {0} добавлен в PATH",
            ),
            // --- index / sync ---
            (
                "index_missing",
                "index not found at {0}\nrun `wing sync` first (downloads ~20 MB from the WinGet CDN)",
                "индекс не найден: {0}\nвыполните `wing sync` (загрузка ~20 МБ)",
            ),
            (
                "index_already",
                "index already present ({0}). Use `wing sync --force` to refresh.",
                "индекс уже загружен ({}). Для обновления: `wing sync --force`",
            ),
            ("downloading_index", "downloading the WinGet index...", "загрузка индекса WinGet..."),
            (
                "index_ready",
                "index ready: {0} package(s), {1} manifest(s), {2}",
                "индекс готов: пакетов {0}, манифестов {1}, {2}",
            ),
            ("index_saved", "saved to {0}", "сохранён в {0}"),
            // --- misc ---
            (
                "requesting_admin",
                "  requesting administrator rights...",
                "  запрашиваются права администратора...",
            ),
            (
                "uninstall_retry",
                "  uninstall returned {0}, retrying elevated...",
                "  удаление вернуло {0}, повтор с повышением прав...",
            ),
            (
                "path_update_failed",
                "  could not update PATH: {0}",
                "  не удалось обновить PATH: {0}",
            ),
        ]
    })
}

/// Column of the entry to use: the locale's position in [`LOCALES`].
/// An unknown locale lands on column 0, which is `en-US`.
fn column(locale: &str) -> usize {
    LOCALES.iter().position(|l| *l == locale).unwrap_or(0)
}

/// Translates `key` for `locale` and substitutes placeholders.
///
/// Both `{}` (sequential) and `{0}` (explicit) forms are accepted. Unknown keys
/// return the key itself, so a missing entry is visible in review rather than
/// silently blanking the output.
pub fn tr(locale: &str, key: &str, args: &[&str]) -> String {
    let all = entries();
    let idx = column(locale);
    let entry = all.iter().find(|(k, _, _)| *k == key);

    let template: String = match entry {
        Some(e) => {
            let chosen = if idx == 1 && !e.2.is_empty() {
                e.2
            } else {
                e.1
            };
            if chosen.is_empty() {
                e.1.to_string()
            } else {
                chosen.to_string()
            }
        }
        None => key.to_string(),
    };

    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template.as_str();
    let mut implicit = 0usize;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('}') else {
            out.push_str(rest);
            return out;
        };
        let inner = &rest[1..end];
        let idx_arg = if inner.is_empty() {
            let i = implicit;
            implicit += 1;
            i
        } else {
            match inner.parse::<usize>() {
                Ok(i) => i,
                Err(_) => {
                    out.push_str(inner);
                    rest = &rest[end + 1..];
                    continue;
                }
            }
        };
        if let Some(v) = args.get(idx_arg) {
            out.push_str(v);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Normalises a user supplied locale tag onto one of [`LOCALES`].
pub fn normalize_locale(tag: &str) -> String {
    let t = tag.trim().to_ascii_lowercase().replace('_', "-");
    let base = t.split('-').next().unwrap_or("");
    for l in LOCALES {
        let lower = l.to_ascii_lowercase();
        let lbase = lower.split('-').next().unwrap_or("");
        if t == lower || base == lbase {
            return (*l).to_string();
        }
    }
    DEFAULT_LOCALE.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_requested_language() {
        assert_eq!(tr("en-US", "error", &[]), "error");
        assert_eq!(tr("ru-RU", "error", &[]), "ошибка");
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        assert_eq!(tr("zz-ZZ", "error", &[]), "error");
        assert_eq!(tr("", "up_to_date", &[]), "everything is up to date");
    }

    #[test]
    fn unknown_key_returns_key() {
        assert_eq!(tr("ru-RU", "no_such_key_here", &[]), "no_such_key_here");
    }

    #[test]
    fn substitutes_numbered_and_positional_arguments() {
        assert_eq!(
            tr("en-US", "not_in_index", &["Foo.Bar"]),
            "`Foo.Bar` is not in the index"
        );
        assert_eq!(
            tr("ru-RU", "not_in_index", &["Foo.Bar"]),
            "`Foo.Bar` отсутствует в индексе"
        );
        assert_eq!(
            tr("en-US", "index_ready", &["15", "181", "41.9 MB"]),
            "index ready: 15 package(s), 181 manifest(s), 41.9 MB"
        );
        assert_eq!(
            tr("ru-RU", "installing", &["Git.Git", "2.55.0"]),
            "установка Git.Git 2.55.0..."
        );
    }

    #[test]
    fn missing_argument_renders_empty() {
        assert_eq!(tr("en-US", "not_in_index", &[]), "`` is not in the index");
    }

    #[test]
    fn locale_normalisation() {
        assert_eq!(normalize_locale("ru"), "ru-RU");
        assert_eq!(normalize_locale("ru_ru"), "ru-RU");
        assert_eq!(normalize_locale("en"), "en-US");
        assert_eq!(normalize_locale("fr-FR"), "en-US");
    }
}

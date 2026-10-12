#!/usr/bin/env bash

# Синк кладёт релизный сквош ПОВЕРХ github develop (без force-push), поэтому
# внешние PR нужно мержить на GitHub ДО релизного синка: их коммиты сохраняют
# авторство в истории зеркала, а содержимое не попадает в сквош повторно —
# rsync-дерево сходится с деревом gitlab, куда те же изменения забраны
# черри-пиком.

set -euo pipefail

# Значения по умолчанию боевые; переопределение существует ради теста: без него
# разрушительный путь — push, тег, gh release edit — не покрыт ничем.
GITHUB_REPO="${GITHUB_SYNC_REPO:-git@github.com:itrous/bsl-analyzer.git}"
GITHUB_REPO_SLUG="${GITHUB_SYNC_REPO_SLUG:-itrous/bsl-analyzer}"
GITHUB_BRANCH="develop"

# Только ОТСЛЕЖИВАЕМЫЕ пути, которым не место в публичном зеркале: дерево берётся
# из HEAD, поэтому неотслеживаемое в зеркало не попадает по построению и
# перечислять его здесь не нужно. Список живёт в файле, а не здесь: по нему же
# тест `mirror_exclusions` ловит код, встраивающий исключённое при компиляции.
EXCLUDE_FILE="$(dirname "${BASH_SOURCE[0]}")/github-mirror-exclude.txt"
if [[ ! -f "$EXCLUDE_FILE" ]]; then
    echo "Нет списка исключений $EXCLUDE_FILE — без него в зеркало уехали бы закрытые пути" >&2
    exit 1
fi
EXCLUDE_PATTERNS=()
while IFS= read -r pattern || [[ -n "$pattern" ]]; do
    # Пробелы по краям срезаются так же, как в тесте `mirror_exclusions`: иначе строка
    # с отступом была бы комментарием для теста и шаблоном для синка.
    pattern="${pattern#"${pattern%%[![:space:]]*}"}"
    pattern="${pattern%"${pattern##*[![:space:]]}"}"
    [[ -z "$pattern" || "$pattern" == \#* ]] && continue
    EXCLUDE_PATTERNS+=("$pattern")
done < "$EXCLUDE_FILE"
if [[ ${#EXCLUDE_PATTERNS[@]} -eq 0 ]]; then
    echo "Список исключений $EXCLUDE_FILE пуст — зеркало унесло бы закрытые пути" >&2
    exit 1
fi

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR=""
DRY_RUN=false
TAG=""
SSH_KEY_FILE=""
NOTES_FILE=""
SKIP_SUMMARY=false
LAST_GH_TAG=""
SOURCE_COMMIT=""
SOURCE_DESCRIBE=""
RELEASE_TARGET=""
STATE_SYNC_REASON=""
FORCE_RELEASE=false
ARCHIVE_PATHSPECS=()


log_info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

cleanup() {
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
    fi
    if [[ -n "$SSH_KEY_FILE" && -f "$SSH_KEY_FILE" ]]; then
        rm -f "$SSH_KEY_FILE"
    fi
    if [[ -n "$NOTES_FILE" && -f "$NOTES_FILE" ]]; then
        rm -f "$NOTES_FILE"
    fi
}
trap cleanup EXIT

get_version() {
    grep -m1 'version = ' "$PROJECT_ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/'
}

# Версия из Cargo.toml отвечает на вопрос «какая версия лежит в дереве», а не «это ли
# дерево выпущено»: между релизами она не меняется. Различает их только тег исходного
# репозитория — на зеркале коммиты сквошенные, и sha там не совпадают по построению.
# Решение принимается один раз: его читают и создание тега, и notes, и сообщение коммита.
classify_release_target() {
    if $FORCE_RELEASE; then
        RELEASE_TARGET="release"
        log_warn "--force-release: тег и notes обновляются без проверки дерева"
        return
    fi

    local tag_commit
    tag_commit=$(git -C "$PROJECT_ROOT" rev-parse --verify --quiet "${COMMIT_TAG}^{commit}" || true)

    if [[ -z "$tag_commit" ]]; then
        RELEASE_TARGET="state-sync"
        STATE_SYNC_REASON="тега $COMMIT_TAG в исходном репозитории нет"
        log_warn "Синк состояния: $STATE_SYNC_REASON — тег и notes на зеркале не трогаю"
        log_warn "(осознанный обход: --force-release)"
        return
    fi

    if [[ "$tag_commit" != "$SOURCE_COMMIT" ]]; then
        RELEASE_TARGET="state-sync"
        STATE_SYNC_REASON="тег $COMMIT_TAG указывает на $(git -C "$PROJECT_ROOT" rev-parse --short "$tag_commit"), а синхронизируется $(git -C "$PROJECT_ROOT" rev-parse --short "$SOURCE_COMMIT")"
        log_warn "Синк состояния: $STATE_SYNC_REASON — тег и notes на зеркале не трогаю"
        log_warn "(осознанный обход: --force-release)"
        return
    fi

    RELEASE_TARGET="release"
    log_info "Дерево совпадает с тегом $COMMIT_TAG — синк релиза"
}

release_commit_subject() {
    if [[ "$RELEASE_TARGET" == release ]]; then
        printf 'Release %s' "$COMMIT_TAG"
    else
        printf 'Sync %s' "$SOURCE_DESCRIBE"
    fi
}

setup_ssh_for_ci() {
    if [[ -z "${GITHUB_SSH_KEY:-}" ]]; then
        return
    fi
    log_info "Настройка SSH для CI..."
    SSH_KEY_FILE=$(mktemp)
    echo "$GITHUB_SSH_KEY" > "$SSH_KEY_FILE"
    chmod 600 "$SSH_KEY_FILE"
    export GIT_SSH_COMMAND="ssh -i $SSH_KEY_FILE -o StrictHostKeyChecking=no"
}

fetch_last_github_tag() {
    # Значение читает только release-путь. Требовать ради него авторизацию gh в синке
    # состояния нельзя: без gh режим работает, и падать с gh без токена он не должен.
    if [[ "$RELEASE_TARGET" != release ]]; then
        return
    fi
    if ! command -v gh >/dev/null 2>&1; then
        log_warn "gh CLI не найден — пропускаем определение последнего тега"
        return
    fi
    if ! gh auth status >/dev/null 2>&1; then
        log_error "gh не авторизован — запустите 'gh auth login' или установите GH_TOKEN"
        exit 1
    fi

    local out rc
    out=$(gh release list \
        --repo "$GITHUB_REPO_SLUG" \
        --limit 10 \
        --json tagName --jq '.[].tagName' 2>&1) && rc=0 || rc=$?
    if (( rc != 0 )); then
        log_error "gh release list завершился с ошибкой (rc=$rc): $out"
        exit 1
    fi

    # База берётся мимо самого синхронизируемого тега: повторный синк уже выпущенного
    # релиза сравнивал бы его с самим собой и вырождался в compare/<tag>...<tag>.
    # Отбор идёт здесь, а не в '--jq', чтобы его исполнял этот скрипт, а не gh.
    local candidate
    while IFS= read -r candidate; do
        if [[ -n "$candidate" && "$candidate" != "$COMMIT_TAG" ]]; then
            LAST_GH_TAG="$candidate"
            break
        fi
    done <<<"$out"
    if [[ -n "$LAST_GH_TAG" ]]; then
        log_info "Последний релиз на GitHub: $LAST_GH_TAG"
    else
        log_info "На GitHub ещё нет релизов"
    fi
}

generate_release_notes() {
    local new_tag="$1"
    local base_tag="$2"

    NOTES_FILE=$(mktemp)

    if [[ "$RELEASE_TARGET" != release ]]; then
        log_info "Синк состояния — release notes не генерируем"
        : > "$NOTES_FILE"
        return
    fi

    # Отбор базы уже исключил сам тег, поэтому сработать это не может: срабатывание
    # означает ошибку отбора, и тогда пустые notes лучше сравнения тега с самим собой.
    if [[ "$base_tag" == "$new_tag" ]]; then
        log_warn "База notes совпала с $new_tag — сравнивать не с чем, notes пустые"
        : > "$NOTES_FILE"
        return
    fi

    if $SKIP_SUMMARY; then
        log_info "Флаг --no-summary — пропускаем генерацию release notes"
        : > "$NOTES_FILE"
        return
    fi

    if ! command -v codex >/dev/null 2>&1; then
        log_warn "codex CLI не найден — release notes будут пустыми"
        : > "$NOTES_FILE"
        return
    fi

    local git_args=(log --no-merges --format='- %s')
    if [[ -n "$base_tag" ]] && git -C "$PROJECT_ROOT" rev-parse --verify --quiet "$base_tag^{commit}" >/dev/null; then
        git_args+=("${base_tag}..HEAD")
    else
        if [[ -n "$base_tag" ]]; then
            log_warn "Базовый тег '$base_tag' недоступен локально — используем всю историю до HEAD"
        fi
        git_args+=(HEAD)
    fi

    local commits
    commits=$(git -C "$PROJECT_ROOT" "${git_args[@]}" 2>/dev/null || true)
    if [[ -z "$commits" ]]; then
        log_warn "Нет коммитов для summary — release notes будут пустыми"
        : > "$NOTES_FILE"
        return
    fi

    commits=$(printf '%s\n' "$commits" | sed -e 's|</commits>|[/commits]|g; s|<commits>|[commits]|g')

    log_info "Генерация release notes через codex ($(echo "$commits" | wc -l) коммитов)..."

    local prompt
    prompt=$(cat <<'EOP'
Ты готовишь release notes для bsl-analyzer на русском языке.

Формат ответа (markdown, без лишних комментариев):
- одно-два предложения общего описания релиза,
- далее маркированный список ключевых изменений, сгруппированных по разделам
  (например: "Производительность", "Новые возможности", "Исправления",
  "Рефакторинг", "Документация") — только те разделы, где есть что написать,
- каждая строка списка — одна краткая фраза.

Используй только данные из списка коммитов ниже, не выдумывай.
Не добавляй заголовок "Release vX.Y.Z" и приветственные/заключительные фразы.

<commits>
EOP
    )
    prompt+=$'\n'"$commits"$'\n'"</commits>"

    local codex_log codex_rc
    codex_log=$(mktemp)
    codex_rc=0
    printf '%s' "$prompt" | timeout 300 codex exec \
            --sandbox read-only \
            --skip-git-repo-check \
            --ephemeral \
            --color never \
            --output-last-message "$NOTES_FILE" \
            - >"$codex_log" 2>&1 || codex_rc=$?
    if (( codex_rc == 124 )); then
        log_warn "codex exec превысил таймаут (300s), release notes будут пустыми"
        log_warn "Лог codex: $codex_log"
        : > "$NOTES_FILE"
        return
    fi
    if (( codex_rc != 0 )); then
        log_warn "codex exec завершился с ошибкой (rc=$codex_rc), release notes будут пустыми"
        log_warn "Лог codex: $codex_log"
        : > "$NOTES_FILE"
        return
    fi
    rm -f "$codex_log"

    sed -i '1{/^```/d}; ${/^```$/d}' "$NOTES_FILE"

    if [[ ! -s "$NOTES_FILE" ]]; then
        log_warn "codex вернул пустой ответ — release notes будут пустыми"
        return
    fi

    log_ok "Release notes сгенерированы ($(wc -l < "$NOTES_FILE") строк)"
}

compose_release_body() {
    local new_tag="$1"
    local base_tag="$2"
    local out="$3"

    if [[ -s "$NOTES_FILE" ]]; then
        cat "$NOTES_FILE" > "$out"
        [[ -z "$(tail -c 1 "$out")" ]] || printf '\n' >> "$out"
    else
        : > "$out"
    fi

    if [[ -n "$base_tag" && "$base_tag" != "$new_tag" ]]; then
        {
            [[ -s "$out" ]] && printf '\n'
            printf '**Full Changelog**: https://github.com/%s/compare/%s...%s\n' \
                "$GITHUB_REPO_SLUG" "$base_tag" "$new_tag"
        } >> "$out"
    fi
}

persist_notes_for_manual_publish() {
    local tag="$1"
    local body_file="$2"
    local persisted="$PROJECT_ROOT/target/release-notes-${tag}.md"

    mkdir -p "$(dirname "$persisted")"
    cp "$body_file" "$persisted"
    log_warn "Notes сохранены: $persisted"
    log_warn "Опубликовать вручную: gh release edit $tag --repo $GITHUB_REPO_SLUG --notes-file '$persisted'"
}

publish_release_notes() {
    local tag="$1"
    local body_file="$2"

    if [[ ! -s "$body_file" ]]; then
        log_info "Тело релиза пустое — пропускаем публикацию notes"
        return
    fi
    if ! command -v gh >/dev/null 2>&1; then
        log_warn "gh CLI не найден — release notes не опубликованы"
        persist_notes_for_manual_publish "$tag" "$body_file"
        return
    fi

    log_info "Ожидаем появления релиза $tag на GitHub (CI собирает артефакты)..."
    local attempt=0
    local max_attempts=60
    while (( attempt < max_attempts )); do
        if gh release view "$tag" --repo "$GITHUB_REPO_SLUG" >/dev/null 2>&1; then
            log_info "Релиз $tag найден, обновляем описание..."
            if gh release edit "$tag" \
                    --repo "$GITHUB_REPO_SLUG" \
                    --notes-file "$body_file" >/dev/null; then
                log_ok "Release notes обновлены: https://github.com/$GITHUB_REPO_SLUG/releases/tag/$tag"
                return
            fi
            log_warn "gh release edit упал, попробуем ещё раз..."
        fi
        attempt=$((attempt + 1))
        sleep 30
    done

    log_warn "Релиз $tag не появился за ${max_attempts} попыток — release notes не обновлены"
    persist_notes_for_manual_publish "$tag" "$body_file"
}

build_archive_pathspecs() {
    ARCHIVE_PATHSPECS=(".")
    for pattern in "${EXCLUDE_PATTERNS[@]}"; do
        # Завершающий слэш git в pathspec не понимает: каталог задаётся своим именем.
        ARCHIVE_PATHSPECS+=(":(exclude)${pattern%/}")
    done
}

assert_tree_committed() {
    # Тем же набором путей, что и архив: правка в исключённом файле состав зеркала
    # не меняет ни в каком состоянии коммита, и блокировать из-за неё нечего.
    if [[ -n "$(git -C "$PROJECT_ROOT" status --porcelain --untracked-files=no -- "${ARCHIVE_PATHSPECS[@]}")" ]]; then
        log_error "В рабочем дереве есть незакоммиченные изменения файлов, попадающих в зеркало."
        log_error "Зеркало собирается из коммита, и эти правки в него не войдут — закоммитьте или откатите их."
        exit 1
    fi
}

sync_files() {
    local src="$1"
    local dst="$2"

    log_info "Синхронизация файлов..."

    find "$dst" -mindepth 1 -maxdepth 1 ! -name '.git' -exec rm -rf {} +

    # Дерево коммита, а не файловая система: копирование с диска утаскивало в публичное
    # зеркало любой локальный каталог, не попавший в список исключений.
    git -C "$src" archive "$SOURCE_COMMIT" -- "${ARCHIVE_PATHSPECS[@]}" | tar -x -C "$dst"
}


for arg in "$@"; do
    case "$arg" in
        --dry-run)
            DRY_RUN=true
            ;;
        --no-summary)
            SKIP_SUMMARY=true
            ;;
        --force-release)
            FORCE_RELEASE=true
            ;;
        v*)
            TAG="$arg"
            ;;
        *)
            log_error "Неизвестный аргумент: $arg"
            exit 1
            ;;
    esac
done


VERSION=$(get_version)
if [[ -n "$TAG" ]]; then
    COMMIT_TAG="$TAG"
else
    COMMIT_TAG="v$VERSION"
fi

log_info "Проект: bsl-analyzer"
log_info "Версия: $VERSION"
log_info "Тег: $COMMIT_TAG"
log_info "GitHub: $GITHUB_REPO"

# HEAD разрешается ОДИН раз: дальше идут сетевые вызовы и генерация release notes, а
# ветка тем временем может уехать под чужим коммитом — зеркало обязано соответствовать
# тому дереву, которое названо здесь.
SOURCE_COMMIT=$(git -C "$PROJECT_ROOT" rev-parse HEAD)
log_info "Источник дерева: $(git -C "$PROJECT_ROOT" rev-parse --short "$SOURCE_COMMIT")"

# Считается здесь, а не на месте сборки сообщения коммита: там уже сделан cd в клон
# зеркала, где этого sha нет — сквош истории исходных коммитов не содержит.
SOURCE_DESCRIBE=$(git -C "$PROJECT_ROOT" describe --tags --always "$SOURCE_COMMIT")

build_archive_pathspecs
assert_tree_committed
classify_release_target
fetch_last_github_tag
generate_release_notes "$COMMIT_TAG" "$LAST_GH_TAG"

RELEASE_BODY_FILE=$(mktemp)
# shellcheck disable=SC2064
trap "rm -f '$RELEASE_BODY_FILE'; cleanup" EXIT
# Только для релиза: строка Full Changelog дописывается независимо от пустоты notes,
# и в сообщении синк-коммита это была бы релизная ссылка на нерелизное дерево.
if [[ "$RELEASE_TARGET" == release ]]; then
    compose_release_body "$COMMIT_TAG" "$LAST_GH_TAG" "$RELEASE_BODY_FILE"
fi

if $DRY_RUN; then
    log_warn "DRY RUN — изменения не будут отправлены"
    echo ""
    log_info "Исключаемые отслеживаемые пути:"
    for pattern in "${EXCLUDE_PATTERNS[@]}"; do
        echo "  - $pattern"
    done
    echo ""
    # Отдельным шагом, чтобы сбой git archive или tar ронял превью, а не выдавал ноль:
    # законный ненулевой статус здесь один — grep -c при пустом счёте.
    DRY_LIST=$(git -C "$PROJECT_ROOT" archive "$SOURCE_COMMIT" -- "${ARCHIVE_PATHSPECS[@]}" | tar -t)
    log_info "Файлов уедет в зеркало: $(printf '%s\n' "$DRY_LIST" | grep -cv '/$' || true)"
    echo ""
    log_info "Режим: $RELEASE_TARGET"
    log_info "Будет создан коммит: \"$(release_commit_subject)\""
    if [[ "$RELEASE_TARGET" != release ]]; then
        log_info "Тег $COMMIT_TAG и release notes на зеркале не трогаются"
    fi
    if [[ -s "$RELEASE_BODY_FILE" ]]; then
        echo ""
        log_info "Release notes:"
        sed 's/^/  | /' "$RELEASE_BODY_FILE"
    fi
    exit 0
fi

setup_ssh_for_ci

WORK_DIR=$(mktemp -d)
log_info "Рабочая директория: $WORK_DIR"

log_info "Клонирование GitHub-репозитория..."
if git ls-remote "$GITHUB_REPO" HEAD &>/dev/null 2>&1; then
    REMOTE_HEAD=$(git ls-remote "$GITHUB_REPO" HEAD 2>/dev/null | awk '{print $1}')
    if [[ -n "$REMOTE_HEAD" ]]; then
        git clone --depth 1 --branch "$GITHUB_BRANCH" "$GITHUB_REPO" "$WORK_DIR/github" 2>/dev/null || {
            log_info "Ветка $GITHUB_BRANCH не найдена, создаём новую..."
            mkdir -p "$WORK_DIR/github"
            cd "$WORK_DIR/github"
            git init
            git checkout --orphan "$GITHUB_BRANCH"
            git remote add origin "$GITHUB_REPO"
        }
    else
        log_info "Пустой репозиторий, создаём начальный коммит..."
        mkdir -p "$WORK_DIR/github"
        cd "$WORK_DIR/github"
        git init
        git checkout --orphan "$GITHUB_BRANCH"
        git remote add origin "$GITHUB_REPO"
    fi
else
    log_error "Не удалось подключиться к $GITHUB_REPO"
    exit 1
fi

cd "$WORK_DIR/github"

sync_files "$PROJECT_ROOT" "$WORK_DIR/github"

cp "$PROJECT_ROOT/crates/bsl-launcher/release-source.github.json" \
   "$WORK_DIR/github/crates/bsl-launcher/release-source.json"

sed -i.bak 's/^INSTALL_SOURCE="gitlab"/INSTALL_SOURCE="github"/' \
    "$WORK_DIR/github/scripts/install.sh"
rm -f "$WORK_DIR/github/scripts/install.sh.bak"

sed -i.bak "s/^\$InstallSource = 'gitlab'/\$InstallSource = 'github'/" \
    "$WORK_DIR/github/scripts/install.ps1"
rm -f "$WORK_DIR/github/scripts/install.ps1.bak"

sed -i.bak '/<!-- INSTALL_URL:gitlab -->/,/<!-- \/INSTALL_URL -->/c\
<!-- INSTALL_URL:github -->\
```bash\
curl -fsSL https://raw.githubusercontent.com/'"$GITHUB_REPO_SLUG"'/develop/scripts/install.sh | bash\
```\
\
Или с указанием версии:\
\
```bash\
curl -fsSL https://raw.githubusercontent.com/'"$GITHUB_REPO_SLUG"'/develop/scripts/install.sh | bash -s -- --version 0.1.38\
```\
<!-- /INSTALL_URL -->' "$WORK_DIR/github/README.md"
rm -f "$WORK_DIR/github/README.md.bak"

sed -i.bak '/<!-- INSTALL_URL_WINDOWS:gitlab -->/,/<!-- \/INSTALL_URL_WINDOWS -->/c\
<!-- INSTALL_URL_WINDOWS:github -->\
```powershell\
irm https://raw.githubusercontent.com/'"$GITHUB_REPO_SLUG"'/develop/scripts/install.ps1 | iex\
```\
\
Или с указанием версии:\
\
```powershell\
\& ([scriptblock]::Create((irm https://raw.githubusercontent.com/'"$GITHUB_REPO_SLUG"'/develop/scripts/install.ps1))) -Version <version>\
```\
<!-- /INSTALL_URL_WINDOWS -->' "$WORK_DIR/github/README.md"
rm -f "$WORK_DIR/github/README.md.bak"

git add -A
if git diff --cached --quiet 2>/dev/null; then
    log_ok "Нет изменений для синхронизации"
    if [[ "$RELEASE_TARGET" == release ]]; then
        if git ls-remote --tags origin "$COMMIT_TAG" | grep -q "$COMMIT_TAG"; then
            log_warn "Тег $COMMIT_TAG уже существует, пропускаем создание"
        else
            git tag -a "$COMMIT_TAG" -m "Release $COMMIT_TAG"
            git push origin "$COMMIT_TAG"
            log_ok "Тег $COMMIT_TAG создан"
        fi
    fi
    exit 0
fi

log_info "Изменения:"
git diff --cached --stat | tail -1

COMMIT_MSG_FILE=$(mktemp)
{
    printf '%s\n' "$(release_commit_subject)"
    if [[ -s "$RELEASE_BODY_FILE" ]]; then
        printf '\n'
        cat "$RELEASE_BODY_FILE"
    fi
} > "$COMMIT_MSG_FILE"
git commit -F "$COMMIT_MSG_FILE" \
    --author="BSL Analyzer <itrous@gmail.com>"
rm -f "$COMMIT_MSG_FILE"

log_info "Отправка в GitHub..."
git push -u origin "$GITHUB_BRANCH"

if [[ "$RELEASE_TARGET" != release ]]; then
    log_warn "Синк состояния — тег $COMMIT_TAG не создаём"
elif git ls-remote --tags origin "$COMMIT_TAG" | grep -q "$COMMIT_TAG"; then
    log_warn "Тег $COMMIT_TAG уже существует, пропускаем создание"
else
    git tag -a "$COMMIT_TAG" -m "Release $COMMIT_TAG"
    git push origin "$COMMIT_TAG"
    log_ok "Тег $COMMIT_TAG создан"
fi

if [[ "$RELEASE_TARGET" == release ]]; then
    publish_release_notes "$COMMIT_TAG" "$RELEASE_BODY_FILE"
else
    log_warn "Синк состояния: $STATE_SYNC_REASON — release notes не трогаю"
fi

log_ok "Синхронизация завершена!"
log_ok "Коммит: $(release_commit_subject)"
log_ok "URL: https://github.com/itrous/bsl-analyzer"

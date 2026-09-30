/*
 * Rich text for the web UI: markdown, syntax-highlighted code and JSON.
 *
 * Everything is built as DOM nodes holding text, never as an HTML string: tool results
 * come from strangers (emails, web pages, transcripts) and the page must never parse
 * them as markup. Links are kept only for http(s) and mailto, and open in a new tab.
 * Loaded before app.js, whose h() helper it uses.
 */

"use strict";

// Beyond these sizes text is shown as is: a huge log is not worth parsing.
const RICH_MAX_CHARS = 200_000;
const HIGHLIGHT_MAX_CHARS = 100_000;

// ---------------------------------------------------------------- choosing a view

/**
 * A tool result, tool arguments or any other value from the server, shown the way it
 * reads best: JSON colour-coded, markdown rendered, everything else as plain text.
 *
 * `markdown: false` keeps strings verbatim (tool arguments: an email body is sent as
 * written, not rendered).
 */
function richValue(value, options = {}) {
    if (typeof value === "string") return richString(value, options);
    if (value === null || typeof value !== "object") return codeView(JSON.stringify(value), "json");
    const entries = Array.isArray(value) ? [] : Object.entries(value);
    // A lone text field, like a command plugin's {"output": "…"}: just the text.
    if (entries.length === 1 && isLongString(entries[0][1]) && !CODE_KEYS[entries[0][0].toLowerCase()]) return richString(entries[0][1], options);
    if (entries.some(([, field]) => isLongString(field))) {
        return switchable(fieldsView(value, options), JSON.stringify(value, null, 2));
    }
    return codeView(JSON.stringify(value, null, 2), "json");
}

/** Text the model wrote (its messages, questions, outcome): always markdown. */
function richMarkdown(text) {
    if (text.length > RICH_MAX_CHARS) return h("div", { class: "txt" }, text);
    return switchable(markdownView(text), text);
}

function richString(text, options) {
    if (text.length > RICH_MAX_CHARS) return plainView(text);
    const trimmed = text.trim();
    if (/^[[{]/.test(trimmed) && /[\]}]$/.test(trimmed)) {
        try {
            return richValue(JSON.parse(trimmed), options);
        } catch {
            // Not JSON after all.
        }
    }
    if (options.markdown !== false && looksLikeMarkdown(text)) return switchable(markdownView(text), text);
    return plainView(text);
}

function isLongString(value) {
    return typeof value === "string" && (value.includes("\n") || value.length > 160);
}

const MARKDOWN_SIGNS = [
    /^ {0,3}#{1,6}[ \t]+\S/m, // heading
    /^ {0,3}(?:```|~~~)/m, // code fence
    /^ {0,3}\|?[^\n|]*\|[^\n]*\n {0,3}\|?[ \t]*:?-{3,}:?[ \t]*(?:\|[ \t]*:?-{3,}:?[ \t]*)*\|?[ \t]*$/m, // table
    /\*\*[^*\n]+\*\*/, // bold
    /\[[^\]\n]+\]\((?:https?:|mailto:)[^)\s]+\)/, // link
];

/** Whether text reads as markdown rather than plain output. */
function looksLikeMarkdown(text) {
    if (MARKDOWN_SIGNS.some((sign) => sign.test(text))) return true;
    const listItems = text.match(/^ {0,3}(?:[-*+]|\d{1,3}[.)])[ \t]+\S/gm) || [];
    const codeSpans = text.match(/`[^`\n]+`/g) || [];
    return listItems.length >= 2 || codeSpans.length >= 2;
}

/** A rendered view with a button that shows the exact text instead. */
function switchable(rendered, original) {
    const raw = h("pre", { class: "raw" }, original);
    raw.hidden = true;
    const button = h(
        "button",
        {
            class: "rich-toggle",
            type: "button",
            title: "Show the exact text",
            onclick: () => {
                const showRaw = raw.hidden;
                raw.hidden = !showRaw;
                rendered.hidden = showRaw;
                button.textContent = showRaw ? "formatted" : "raw";
                button.title = showRaw ? "Show it formatted" : "Show the exact text";
            },
        },
        "raw",
    );
    return h("div", { class: "rich-switch" }, button, rendered, raw);
}

function plainView(text) {
    return h("pre", { class: "raw" }, text);
}

/** Code in a scrolling block, colour-coded when the language is known. */
function codeView(code, lang) {
    return h("pre", { class: "raw code" }, highlight(code, lang));
}

// Keys whose string value is code in a known language (tool arguments).
const CODE_KEYS = { command: "bash", cmd: "bash", script: "bash", query: "sql", sql: "sql" };

/** An object with long text in it: one row per key, the text shown as text, not escaped. */
function fieldsView(object, options) {
    return h(
        "dl",
        { class: "fields" },
        Object.entries(object).map(([key, value]) => {
            let shown;
            if (typeof value === "string" && isLongString(value)) {
                const lang = CODE_KEYS[key.toLowerCase()];
                if (lang) shown = codeView(value, lang);
                else if (options.markdown === false) shown = plainView(value);
                else shown = looksLikeMarkdown(value) ? h("div", { class: "rich" }, markdownBlocks(value)) : plainView(value);
            } else if (typeof value === "string") {
                shown = h("span", { class: "field-text" }, value);
            } else {
                shown = h("code", { class: "field-code" }, highlight(JSON.stringify(value), "json"));
            }
            return [h("dt", {}, key), h("dd", {}, shown)];
        }),
    );
}

// ---------------------------------------------------------------- markdown: blocks

function markdownView(text) {
    return h("div", { class: "rich" }, markdownBlocks(text));
}

const FENCE = /^( {0,3})(`{3,}|~{3,})[ \t]*([^\s`]*)[^`]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}> ?/;
const LIST_ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])(?:([ \t]+)(.*)|$)/;
const TABLE_DELIMITER = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;

/** Markdown text as a list of block elements. */
function markdownBlocks(text) {
    const lines = text.replace(/\r\n?/g, "\n").split("\n").map(expandTabs);
    return blocks(lines);
}

function expandTabs(line) {
    return line.replace(/^[ \t]+/, (indent) => indent.replace(/\t/g, "    "));
}

function indentOf(line) {
    return line.length - line.trimStart().length;
}

function isBlank(line) {
    return line.trim() === "";
}

function isTableStart(lines, i) {
    return lines[i].includes("|") && i + 1 < lines.length && lines[i + 1].includes("-") && TABLE_DELIMITER.test(lines[i + 1]);
}

/** Whether a line ends the paragraph above it. */
function interruptsParagraph(lines, i) {
    const line = lines[i];
    if (FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || isTableStart(lines, i)) return true;
    const item = LIST_ITEM.exec(line);
    // Like CommonMark: a numbered list interrupts a paragraph only when it starts at 1.
    return Boolean(item && item[4] && item[4].trim() && (/^[-*+]$/.test(item[2]) || /^1[.)]$/.test(item[2])));
}

function blocks(lines) {
    const out = [];
    let i = 0;
    while (i < lines.length) {
        const line = lines[i];
        if (isBlank(line)) {
            i++;
            continue;
        }
        const fence = FENCE.exec(line);
        if (fence) {
            const [, indent, marker, lang] = fence;
            const close = new RegExp(`^ {0,3}${marker[0] === "`" ? "`" : "~"}{${marker.length},}[ \\t]*$`);
            const code = [];
            i++;
            while (i < lines.length && !close.test(lines[i])) {
                code.push(lines[i].slice(Math.min(indent.length, indentOf(lines[i]))));
                i++;
            }
            i++; // the closing fence, or past the end when it is missing
            out.push(codeBlock(code.join("\n"), lang));
            continue;
        }
        const heading = HEADING.exec(line);
        if (heading) {
            out.push(h(`h${heading[1].length}`, {}, inline(heading[2] || "")));
            i++;
            continue;
        }
        if (RULE.test(line)) {
            out.push(h("hr"));
            i++;
            continue;
        }
        if (QUOTE.test(line)) {
            const quoted = [];
            while (i < lines.length && QUOTE.test(lines[i])) {
                quoted.push(lines[i].replace(QUOTE, ""));
                i++;
            }
            out.push(h("blockquote", {}, blocks(quoted)));
            continue;
        }
        if (isTableStart(lines, i)) {
            const [node, next] = table(lines, i);
            out.push(node);
            i = next;
            continue;
        }
        const item = LIST_ITEM.exec(line);
        if (item) {
            const [node, next] = list(lines, i);
            out.push(node);
            i = next;
            continue;
        }
        const paragraph = [line.trim()];
        i++;
        while (i < lines.length && !isBlank(lines[i]) && !interruptsParagraph(lines, i)) {
            paragraph.push(lines[i].trim());
            i++;
        }
        out.push(h("p", {}, inline(paragraph.join("\n"))));
    }
    return out;
}

/** A list and everything nested in it; returns the element and the next line to read. */
function list(lines, start) {
    const first = LIST_ITEM.exec(lines[start]);
    const ordered = !/^[-*+]$/.test(first[2]);
    const baseIndent = first[1].length;
    const items = [];
    let loose = false;
    let i = start;
    while (i < lines.length) {
        const match = LIST_ITEM.exec(lines[i]);
        if (!match || match[1].length !== baseIndent || ordered === /^[-*+]$/.test(match[2])) break;
        const gap = match[3] ? match[3].length : 1;
        // Content lines are indented past the marker; accept a little less, as models
        // often nest with two spaces under "1.".
        const contentIndent = baseIndent + match[2].length + (gap > 4 ? 1 : gap);
        const nestIndent = Math.min(contentIndent, baseIndent + 2);
        const body = [gap > 4 ? " ".repeat(gap - 1) + (match[4] || "") : match[4] || ""];
        i++;
        while (i < lines.length) {
            const next = lines[i];
            if (isBlank(next)) {
                // A blank line continues the item only if indented content follows.
                let j = i;
                while (j < lines.length && isBlank(lines[j])) j++;
                if (j < lines.length && indentOf(lines[j]) >= nestIndent) {
                    body.push(...lines.slice(i, j).map(() => ""));
                    loose = true;
                    i = j;
                    continue;
                }
                break;
            }
            if (indentOf(next) >= nestIndent) {
                body.push(next.slice(Math.min(contentIndent, indentOf(next))));
                i++;
                continue;
            }
            // A lazy continuation of the item's paragraph.
            if (!isBlank(lines[i - 1]) && !interruptsParagraph(lines, i) && !LIST_ITEM.test(next)) {
                body.push(next.trim());
                i++;
                continue;
            }
            break;
        }
        items.push({ number: match[2], body });
        // Blank lines between items make the list loose.
        if (i < lines.length && isBlank(lines[i])) {
            let j = i;
            while (j < lines.length && isBlank(lines[j])) j++;
            const following = j < lines.length && LIST_ITEM.exec(lines[j]);
            if (following && following[1].length === baseIndent && ordered !== /^[-*+]$/.test(following[2])) {
                loose = true;
                i = j;
            }
        }
    }
    const props = {};
    if (ordered) {
        const number = parseInt(first[2], 10);
        if (number !== 1) props.start = String(number);
    }
    return [h(ordered ? "ol" : "ul", props, items.map((item) => listItem(item.body, loose))), i];
}

function listItem(body, loose) {
    let task = null;
    const check = /^\[([ xX])\][ \t]+/.exec(body[0]);
    if (check) {
        task = check[1] !== " ";
        body = [body[0].slice(check[0].length), ...body.slice(1)];
    }
    let children = blocks(body);
    // A tight list keeps its text out of paragraphs.
    if (!loose) children = children.flatMap((child) => (child.tagName === "P" ? [...child.childNodes] : [child]));
    const props = task === null ? {} : { class: "task" };
    return h("li", props, task !== null && h("input", { type: "checkbox", checked: task, disabled: true, "aria-label": task ? "done" : "not done" }), children);
}

function splitRow(line) {
    let row = line.trim();
    if (row.startsWith("|")) row = row.slice(1);
    if (row.endsWith("|") && !row.endsWith("\\|")) row = row.slice(0, -1);
    const cells = [];
    let cell = "";
    let inCode = false;
    for (let k = 0; k < row.length; k++) {
        const ch = row[k];
        if (ch === "\\" && row[k + 1] === "|") {
            cell += "|";
            k++;
        } else if (ch === "`") {
            inCode = !inCode;
            cell += ch;
        } else if (ch === "|" && !inCode) {
            cells.push(cell.trim());
            cell = "";
        } else {
            cell += ch;
        }
    }
    cells.push(cell.trim());
    return cells;
}

function table(lines, start) {
    const header = splitRow(lines[start]);
    const aligns = splitRow(lines[start + 1]).map((cell) => {
        const left = cell.startsWith(":");
        const right = cell.endsWith(":");
        return left && right ? "center" : right ? "right" : left ? "left" : "";
    });
    const cell = (tag, text, k) => h(tag, aligns[k] ? { class: `align-${aligns[k]}` } : {}, inline(text || ""));
    const rows = [];
    let i = start + 2;
    while (i < lines.length && !isBlank(lines[i]) && lines[i].includes("|")) {
        const cells = splitRow(lines[i]);
        rows.push(h("tr", {}, header.map((_, k) => cell("td", cells[k], k))));
        i++;
    }
    const node = h(
        "div",
        { class: "table-wrap" },
        h("table", {}, h("thead", {}, h("tr", {}, header.map((text, k) => cell("th", text, k)))), rows.length > 0 && h("tbody", {}, rows)),
    );
    return [node, i];
}

function codeBlock(code, lang) {
    const label = lang ? lang.toLowerCase() : "";
    return h(
        "div",
        { class: "codeblock" },
        h("div", { class: "code-bar" }, h("span", {}, label || "code"), copyButton(code)),
        h("pre", { class: "code" }, h("code", {}, highlight(code, label))),
    );
}

function copyButton(text) {
    const button = h(
        "button",
        {
            type: "button",
            class: "copy",
            onclick: () => {
                // Only in a secure context (https or localhost).
                if (!navigator.clipboard) return toast("Copying needs https", "bad");
                navigator.clipboard.writeText(text).then(
                    () => {
                        button.textContent = "copied";
                        setTimeout(() => (button.textContent = "copy"), 1500);
                    },
                    () => toast("Could not copy", "bad"),
                );
            },
        },
        "copy",
    );
    return button;
}

// ---------------------------------------------------------------- markdown: inline

const ESCAPED = /\\([!-/:-@[-`{-~])/y;
// Spans are bounded so text full of unclosed markers stays fast to parse.
const CODE_SPAN = /(`+)([^`]|[^`][\s\S]{0,4000}?[^`])\1(?!`)/y;
const ANGLE_LINK = /<((?:https?:\/\/|mailto:)[^\s<>]+)>/y;
const BARE_URL = /https?:\/\/[^\s<>]*[^\s<>.,:;"'!?\]}*_~]/y;
const BREAK_TAG = /<br\s*\/?>/iy;
const EMPHASIS = [
    [/\*\*\*(?=\S)([\s\S]{0,2000}?\S)\*\*\*/y, (content) => h("strong", {}, h("em", {}, inline(content)))],
    [/\*\*(?=\S)([\s\S]{0,2000}?\S)\*\*/y, (content) => h("strong", {}, inline(content))],
    [/__(?=\S)([\s\S]{0,2000}?\S)__(?![\p{L}\p{N}_])/uy, (content) => h("strong", {}, inline(content))],
    [/\*(?=[^\s*])([\s\S]{0,2000}?[^\s*])\*(?!\*)/y, (content) => h("em", {}, inline(content))],
    [/_(?=[^\s_])([\s\S]{0,2000}?[^\s_])_(?![\p{L}\p{N}_])/uy, (content) => h("em", {}, inline(content))],
    [/~~(?=\S)([\s\S]{0,2000}?\S)~~/y, (content) => h("del", {}, inline(content))],
];
const LINK_TARGET = /\(\s*<?((?:[^\s<>()\\]|\\.|\([^\s()]*\))*)>?(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/y;

/** A bare URL without a closing parenthesis it did not open: "(see https://x.y/a)". */
function balancedUrl(url) {
    let end = url.length;
    const count = (text, ch) => text.split(ch).length - 1;
    while (url[end - 1] === ")" && count(url.slice(0, end), ")") > count(url.slice(0, end), "(")) end--;
    return url.slice(0, end);
}

function safeHref(href) {
    return /^(?:https?:\/\/|mailto:)/i.test(href) ? href : null;
}

function link(href, content) {
    return h("a", { href, target: "_blank", rel: "noopener noreferrer" }, content);
}

function sticky(pattern, text, at) {
    pattern.lastIndex = at;
    return pattern.exec(text);
}

/** The "[label](target)" link or "![alt](src)" image at `at`, if there is one. */
function linkAt(text, at) {
    const image = text[at] === "!";
    let k = at + (image ? 2 : 1);
    let depth = 1;
    const limit = Math.min(text.length, at + 2000);
    while (k < limit && depth > 0) {
        if (text[k] === "\\") k++;
        else if (text[k] === "[") depth++;
        else if (text[k] === "]") depth--;
        k++;
    }
    if (depth !== 0) return null;
    const target = sticky(LINK_TARGET, text, k);
    if (!target) return null;
    const label = text.slice(at + (image ? 2 : 1), k - 1);
    const href = safeHref(target[1].replace(/\\(.)/g, "$1"));
    const content = image ? `🖼 ${label || "image"}` : inline(label);
    // Images are not loaded (the page loads nothing from other sites): a link stands in.
    return { node: href ? link(href, content) : h("span", {}, content), end: k + target[0].length };
}

/** Inline markdown (code, emphasis, links, line breaks) as nodes. */
function inline(text) {
    const out = [];
    let plain = "";
    const flush = () => {
        plain.split("\n").forEach((part, k) => {
            if (k > 0) out.push(h("br"));
            if (part) out.push(part);
        });
        plain = "";
    };
    const push = (node, end) => {
        flush();
        out.push(node);
        return end;
    };
    let i = 0;
    while (i < text.length) {
        const ch = text[i];
        let match;
        if (ch === "\\" && (match = sticky(ESCAPED, text, i))) {
            plain += match[1];
            i += 2;
        } else if (ch === "`" && (match = sticky(CODE_SPAN, text, i))) {
            let code = match[2].replace(/\n/g, " ");
            if (/^ .*[^ ].* $/.test(code)) code = code.slice(1, -1);
            i = push(h("code", {}, code), i + match[0].length);
        } else if (ch === "<" && (match = sticky(ANGLE_LINK, text, i))) {
            i = push(link(match[1], match[1]), i + match[0].length);
        } else if (ch === "<" && (match = sticky(BREAK_TAG, text, i))) {
            i = push(h("br"), i + match[0].length);
        } else if ((ch === "[" || (ch === "!" && text[i + 1] === "[")) && (match = linkAt(text, i))) {
            i = push(match.node, match.end);
        } else if (ch === "h" && !/[\p{L}\p{N}]/u.test(text[i - 1] || "") && (match = sticky(BARE_URL, text, i))) {
            const url = balancedUrl(match[0]);
            i = push(link(url, url), i + url.length);
        } else if ("*_~".includes(ch)) {
            // Underscores inside words (snake_case) are not emphasis.
            const intraword = ch === "_" && /[\p{L}\p{N}]/u.test(text[i - 1] || "");
            const rule = intraword ? null : EMPHASIS.find(([pattern]) => (match = sticky(pattern, text, i)));
            if (rule) {
                i = push(rule[1](match[1]), i + match[0].length);
            } else {
                // Skip the whole run so "***" or "__" is not retried one character later.
                let end = i;
                while (text[end] === ch) end++;
                plain += text.slice(i, end);
                i = end;
            }
        } else {
            plain += ch;
            i++;
        }
    }
    flush();
    return out;
}

// ---------------------------------------------------------------- syntax highlighting

const NUMBER = /-?\b(?:0x[\da-fA-F_]+|0b[01_]+|0o[0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)(?:[a-z]\w*)?\b/y;
const DOUBLE_STRING = /"(?:[^"\\\n]|\\.)*"/y;
const SINGLE_STRING = /'(?:[^'\\\n]|\\.)*'/y;
const LINE_COMMENT = /\/\/.*/y;
const BLOCK_COMMENT = /\/\*[\s\S]*?(?:\*\/|$)/y;
const HASH_COMMENT = /#.*/y;
const CALL = /[A-Za-z_$][\w$]*(?=\s*\()/y;
const TYPE_NAME = /\b[A-Z][\w$]*\b/y;
// A plain word, so keywords never match in the middle of one.
const WORD = /[A-Za-z_$][\w$]*/y;

function words(list, flags = "") {
    return new RegExp(`\\b(?:${list.trim().split(/\s+/).join("|")})\\b`, `y${flags}`);
}

const C_LIKE_KEYWORDS =
    "if else for while do switch case default break continue return goto try catch finally throw throws new delete " +
    "class struct enum union interface extends implements public private protected static const final virtual override " +
    "abstract void int long short char float double bool boolean unsigned signed auto typedef namespace using template " +
    "typename sizeof inline extern volatile register import package var val fun func let this super";

const LANGUAGES = {
    json: [
        ["key", /"(?:[^"\\\n]|\\.)*"(?=\s*:)/y],
        ["str", DOUBLE_STRING],
        ["num", NUMBER],
        ["lit", words("true false null")],
    ],
    javascript: [
        ["com", LINE_COMMENT],
        ["com", BLOCK_COMMENT],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["str", /`(?:[^`\\]|\\[\s\S])*`/y],
        ["kw", words("async await break case catch class const continue debugger default delete do else export extends finally for from function if import in instanceof let new of return static switch throw try typeof var void while with yield type interface enum implements private public protected readonly as")],
        ["lit", words("true false null undefined NaN Infinity this super")],
        ["num", NUMBER],
        ["fn", CALL],
        ["type", TYPE_NAME],
        [null, WORD],
    ],
    python: [
        ["com", HASH_COMMENT],
        ["str", /[rRbBuUfF]{0,2}(?:"""[\s\S]*?(?:"""|$)|'''[\s\S]*?(?:'''|$))/y],
        ["str", /[rRbBuUfF]{0,2}(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')/y],
        ["meta", /@[\w.]+/y],
        ["kw", words("and as assert async await break class continue def del elif else except finally for from global if import in is lambda match case nonlocal not or pass raise return try while with yield")],
        ["lit", words("True False None self cls")],
        ["num", NUMBER],
        ["fn", CALL],
        ["type", TYPE_NAME],
        [null, WORD],
    ],
    rust: [
        ["com", LINE_COMMENT],
        ["com", BLOCK_COMMENT],
        ["meta", /#!?\[[^\]\n]*\]/y],
        ["str", /b?r(#*)"[\s\S]*?"\1/y],
        ["str", /b?"(?:[^"\\]|\\[\s\S])*"/y],
        ["str", /b?'(?:[^'\\\n]|\\[^\n]{1,8})'/y],
        ["type", /'[a-z_]\w*/y],
        ["kw", words("as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return static struct trait type unsafe use where while")],
        ["lit", words("true false self Self None Some Ok Err")],
        ["fn", /[a-z_]\w*!(?!=)/y],
        ["num", NUMBER],
        ["fn", CALL],
        ["type", TYPE_NAME],
        [null, WORD],
    ],
    go: [
        ["com", LINE_COMMENT],
        ["com", BLOCK_COMMENT],
        ["str", DOUBLE_STRING],
        ["str", /`[^`]*`/y],
        ["str", SINGLE_STRING],
        ["kw", words("break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var")],
        ["lit", words("true false nil iota")],
        ["num", NUMBER],
        ["fn", CALL],
        ["type", TYPE_NAME],
        [null, WORD],
    ],
    clike: [
        ["com", LINE_COMMENT],
        ["com", BLOCK_COMMENT],
        ["meta", /#\s*[a-z]+/y],
        ["meta", /@[\w.]+/y],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["kw", words(C_LIKE_KEYWORDS)],
        ["lit", words("true false null nullptr NULL nil")],
        ["num", NUMBER],
        ["fn", CALL],
        ["type", TYPE_NAME],
        [null, WORD],
    ],
    bash: [
        ["com", /(?<=^|[\s;|&(])#.*/my],
        ["meta", /^[ \t]*\$(?= )/my],
        ["str", /'[^']*'/y],
        ["str", /"(?:[^"\\]|\\[\s\S])*"/y],
        ["var", /\$(?:\{[^}\n]*\}|\w+|[@#?$!*-])/y],
        ["kw", words("if then else elif fi for in do done while until case esac function return local export readonly declare set unset shift exit break continue source alias")],
        ["fn", /(?<=^[ \t]*|[|;&(][ \t]*|\b(?:sudo|then|do|else)[ \t]+)[\w./-]+/my],
        ["attr", /(?<=\s)--?[\w-]+/y],
        ["num", /\b\d+\b/y],
        [null, /[\w./-]+/y],
    ],
    toml: [
        ["com", HASH_COMMENT],
        ["meta", /^[ \t]*\[\[?[^\]\n]*\]\]?/my],
        ["key", /^[ \t]*[\w.\-"']+(?=[ \t]*=)/my],
        ["str", /"""[\s\S]*?"""|'''[\s\S]*?'''/y],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["lit", words("true false")],
        ["num", /\d{4}-\d{2}-\d{2}(?:[T ][\d:.]+(?:Z|[+-]\d{2}:\d{2})?)?/y],
        ["num", NUMBER],
        [null, WORD],
    ],
    yaml: [
        ["com", /(?<=^|\s)#.*/my],
        ["meta", /^(?:---|\.\.\.)[ \t]*$/my],
        ["key", /(?<=^[ \t]*(?:-[ \t]+)?)[^\s#:'"-][^\n:#]*?(?=:(?:\s|$))/my],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["meta", /[&*][\w-]+/y],
        ["lit", words("true false null yes no on off")],
        ["num", NUMBER],
        [null, WORD],
    ],
    sql: [
        ["com", /--.*/y],
        ["com", BLOCK_COMMENT],
        ["str", SINGLE_STRING],
        ["str", DOUBLE_STRING],
        ["kw", words("select from where and or not insert into values update set delete create table index view drop alter add column primary key foreign references join inner left right outer full cross on as group by order having limit offset union all distinct case when then else end is null like in between exists returning with begin commit rollback transaction pragma integer text real blob default unique check asc desc count sum avg min max", "i")],
        ["num", NUMBER],
        ["fn", CALL],
        [null, WORD],
    ],
    html: [
        ["com", /<!--[\s\S]*?(?:-->|$)/y],
        ["meta", /<!doctype[^>]*>/iy],
        ["tag", /<\/?[\w:-]+/y],
        ["tag", /\/?>/y],
        ["attr", /[\w:-]+(?==)/y],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
    ],
    css: [
        ["com", BLOCK_COMMENT],
        ["kw", /@[\w-]+/y],
        ["key", /[\w-]+(?=\s*:[^{;]*[;}])/y],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["num", /#[\da-fA-F]{3,8}\b/y],
        ["num", /-?\d*\.?\d+(?:%|[a-z]+)?/y],
        [null, /[\w-]+/y],
    ],
    diff: [
        ["meta", /^(?:@@.*|diff .*|index .*|--- .*|\+\+\+ .*)$/my],
        ["add", /^\+.*$/my],
        ["del", /^-.*$/my],
        [null, /.+/y],
    ],
    dockerfile: [
        ["com", /^[ \t]*#.*/my],
        ["kw", /^[ \t]*(?:FROM|RUN|CMD|LABEL|EXPOSE|ENV|ADD|COPY|ENTRYPOINT|VOLUME|USER|WORKDIR|ARG|ONBUILD|STOPSIGNAL|HEALTHCHECK|SHELL|AS)\b/imy],
        ["str", DOUBLE_STRING],
        ["str", SINGLE_STRING],
        ["var", /\$(?:\{[^}\n]*\}|\w+)/y],
        [null, /[\w./-]+/y],
    ],
};

const LANGUAGE_ALIASES = {
    js: "javascript",
    jsx: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    ts: "javascript",
    tsx: "javascript",
    typescript: "javascript",
    node: "javascript",
    py: "python",
    python3: "python",
    rs: "rust",
    golang: "go",
    c: "clike",
    h: "clike",
    cpp: "clike",
    "c++": "clike",
    cc: "clike",
    hpp: "clike",
    java: "clike",
    cs: "clike",
    csharp: "clike",
    kotlin: "clike",
    kt: "clike",
    swift: "clike",
    php: "clike",
    sh: "bash",
    shell: "bash",
    zsh: "bash",
    console: "bash",
    shellsession: "bash",
    terminal: "bash",
    ini: "toml",
    cfg: "toml",
    conf: "toml",
    env: "toml",
    yml: "yaml",
    xml: "html",
    svg: "html",
    htm: "html",
    vue: "html",
    scss: "css",
    less: "css",
    patch: "diff",
    docker: "dockerfile",
    jsonc: "json",
    json5: "json",
    geojson: "json",
};

/** Code as highlighted spans; unknown languages (and JSON without a tag) are guessed. */
function highlight(code, lang) {
    let name = LANGUAGE_ALIASES[lang] || lang;
    if (!LANGUAGES[name]) name = guessLanguage(code);
    const rules = LANGUAGES[name];
    if (!rules || code.length > HIGHLIGHT_MAX_CHARS) return [code];
    const out = [];
    let plain = "";
    let i = 0;
    while (i < code.length) {
        let matched = false;
        for (const [kind, pattern] of rules) {
            pattern.lastIndex = i;
            const match = pattern.exec(code);
            if (!match || match[0].length === 0) continue;
            if (kind) {
                if (plain) out.push(plain);
                plain = "";
                out.push(h("span", { class: `tok-${kind}` }, match[0]));
            } else {
                plain += match[0];
            }
            i += match[0].length;
            matched = true;
            break;
        }
        if (!matched) {
            plain += code[i];
            i++;
        }
    }
    if (plain) out.push(plain);
    return out;
}

function guessLanguage(code) {
    const trimmed = code.trim();
    if (/^[[{]/.test(trimmed)) {
        try {
            JSON.parse(trimmed);
            return "json";
        } catch {
            // Not JSON.
        }
    }
    if (/^(?:diff --git|--- \S.*\n\+\+\+ )/.test(trimmed)) return "diff";
    if (/^#!.*\b(?:ba|z)?sh\b/.test(trimmed) || /^\$ \S/m.test(trimmed)) return "bash";
    return null;
}

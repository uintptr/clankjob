# Email plugin (IMAP/SMTP)

Lets cases email people and wait for their answers. A case can ask a contractor for a
quote, sleep until the reply arrives (checking your mailbox after 1, 2, 5 and 10 minutes,
then every 15, without using the LLM), read it, and follow up if it doesn't come.

**You approve emails before they go out**: the draft appears in the web inbox and on
Discord (✅/❌); in the web UI you can also edit it first. Emails whose recipients are all
**trusted contacts** (the Contacts page) go out without asking, and each case's
**Approvals** setting can make it always or never ask ([design §9.7](../../docs/design.md)).

| Tool          | What it does                                           | Approval                               |
| ------------- | ------------------------------------------------------ | -------------------------------------- |
| `send_email`  | Sends a new email                                      | yes, unless every recipient is trusted |
| `reply_email` | Replies within a thread (subject and threading kept)   | yes, unless every recipient is trusted |
| `list_emails` | Lists recent emails: sender, subject, date, message_id | no                                     |
| `read_email`  | Reads one email: headers, text, attachment names       | no                                     |

| Wait condition         | Fires when                                               |
| ---------------------- | -------------------------------------------------------- |
| `email_reply_received` | a reply to a given email arrives (matched by headers)    |
| `email_received`       | new email arrives, optionally from someone or by subject |

The `email` guide tells the agent how to write, wait and follow up.

## Setup

1. Create an **app password** for your mailbox (Yahoo and Gmail require one for IMAP/SMTP).

2. `cp config.example.toml config.toml` (git-ignored) and fill in the address and servers.
   Keep the password out of the file: `EMAIL_PASSWORD = { env = "EMAIL_PASSWORD" }`, then
   `export EMAIL_PASSWORD=…` before starting the server.

3. Include your spam folder in `EMAIL_FOLDERS` (Yahoo: `INBOX, Bulk`) so replies filed
   as spam are found.

4. Optional: `EMAIL_ALLOWED_RECIPIENTS` refuses anyone else, even if approved.

5. Check it against your servers (logs in, lists folders; sends nothing):

   ```sh
   read -rs EMAIL_PASSWORD && export EMAIL_PASSWORD
   ./check_config.py
   ```

The server loads the plugin from `plugins_dir` and reloads it when `config.toml` changes;
the Plugins page shows its tools and conditions.

Standard library only (Python 3.11+). Tests run against a fake mailbox:

```sh
python3 -m unittest -v test_email_tool.py
```

## Design

`email_tool.py` is a command plugin ([design §9.9](../../docs/design.md)): standard
library only (`imaplib`, `smtplib`, `email`), one subcommand per tool call or check,
printing JSON. Sending uses approvals ([design §9.7](../../docs/design.md)) and replies
are waited for with plugin wait conditions ([design §6.2](../../docs/design.md)).

### Configuration

`config.toml` (git-ignored; `config.example.toml` is the template) sets environment
variables for the tool through `[env]`, the password as a secret reference:

```toml
[env]
EMAIL_ADDRESS = "joe@example.com"
EMAIL_PASSWORD = { env = "EMAIL_PASSWORD" }     # an app password
EMAIL_IMAP_HOST = "imap.mail.yahoo.com"         # port 993, TLS
EMAIL_SMTP_HOST = "smtp.mail.yahoo.com"         # 465 (TLS) or 587 (STARTTLS)
EMAIL_FOLDERS = "INBOX, Bulk"                   # searched for replies and new mail
# EMAIL_NAME, EMAIL_USERNAME, EMAIL_SENT_FOLDER (copy sent mail), EMAIL_ALLOWED_RECIPIENTS
```

`EMAIL_ALLOWED_RECIPIENTS` (addresses or `@domains`) refuses anyone else even when
approved; it complements approvals, it does not replace them.

### Tools in detail

| Tool          | Args                                              | Result                                                                                                                    | Approval |
| ------------- | ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | -------- |
| `send_email`  | `to`, `subject`, `body`, `cc?`                    | `{ message_id, thread_ref, to, subject }`                                                                                 | yes      |
| `reply_email` | `message_id`, `body`, `all?`                      | `{ message_id, thread_ref, to, subject }`                                                                                 | yes      |
| `list_emails` | `from?`, `since?`, `thread?`, `limit?`, `folder?` | headers only, newest first (last 7 days by default)                                                                       | no       |
| `read_email`  | `message_id`                                      | headers, text body (HTML as text), attachment names, a third-party caution; looked up in every folder, sent mail included | no       |

- Outgoing mail gets a `Message-ID` in the mailbox's own domain. `reply_email` looks the
  original up by Message-ID, replies to its `Reply-To` or `From` (plus To and Cc minus
  ourselves with `all`), keeps `Re:` and sets `In-Reply-To` and `References`.
- Mail is fetched with `BODY.PEEK`, so reading or checking never marks it as read.
- Message-IDs are validated before they reach an IMAP search, and addresses before SMTP.
- An email with template gaps left in its subject or body (`[Your Name]`, `<insert date>`,
  `{name}`, `$XXX`, lorem ipsum) is refused before anything is sent, approved or not; the
  error names them so the agent fills them in or asks the owner. Brackets count only with
  a word such as *name*, *date* or *your* inside, so `[URGENT]`, `[sic]` and
  `Bob <bob@sparky.ca>` go through.
- Sending is protected by approvals and the at-most-once execution of [design §9.7](../../docs/design.md); a crash
  mid-send is reported as "outcome unknown", never sent again. Planned: attaching case
  files, and importing attachments of incoming mail as case files (marked third-party).

### Wait conditions in detail

| Kind                   | Params                       | Fires when                                                                                                                     |
| ---------------------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `email_reply_received` | `message_id`                 | a message in the configured folders has `message_id` in its `In-Reply-To` or `References`, from any sender                     |
| `email_received`       | `from?`, `subject_contains?` | a new message arrives, counted from the first check (the cursor keeps the last UID per folder, reset if `UIDVALIDITY` changes) |

Replies are matched by their threading headers only, never by sender: people often
answer from another address or through a forward (the first real test was answered from
a different address than the one written to, and a sender filter hid the reply).

Both are checked right away, then 1, 2, 5 and 10 minutes apart, then every 15 minutes
(the server's ramp, [design §6.2](../../docs/design.md)); `check_every` can change the
interval, down to 1 minute. The case waits on
the Message-ID of the email it sent **last**: a reply to that message always references
it, while earlier replies in the thread do not, so an old reply can never fire the
condition again and no state is needed. Events carry `message_id`, `from`, `subject`,
`date` and `folder`; the LLM reads the body with `read_email`.

The `email` guide tells the LLM how to write, wait (with a timeout for following up),
treat replies as third-party content, and follow up once before asking the owner.

### Inbound email that starts a case (later)

A plugin could later spawn cases from inbound mail (`create_cases_from: {folder, filter, template}`), reusing the same checks on a per-plugin schedule.

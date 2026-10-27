# Tool catalog: `ai.generic`

Generated from the code; do not edit it. A test in `nodes/ai/ai-generic` checks this page and [agent-tools.json](agent-tools.json) against the tools' cards, and CI runs it. After changing a card, write both again:

```bash
UPDATE_AGENT_TOOLS_CATALOG=1 cargo test -p fluxbee-ai-nodes --bin ai_node_runner the_tool_catalog_matches_the_cards
```

- **What it is:** every base tool of the runtime, as its card declares it ([agent-tools-v1](../agent-tools-v1.md)). Every AI agent loads every base tool; a message gets one when its *present when* holds.
- **The same cards** are what the model receives (the summary, when to use the tool, its limits and what it returns, with the input schema), what an AI node answers to `NODE_HELP`, and `dist/runtimes/ai.generic/<version>/tools.json` in an installation. The repository's `agent-tools.json` has `runtime_version: null`: the package writes the version it ships.
- **A failure** is a result `{"status": "error", "error_code", "message", "retryable"}` with one of the tool's codes. Arguments that do not fit the input schema, or a failure inside the tool, come back instead as an error result: a text that names the problem.

| Tool | What it does | Present when |
| --- | --- | --- |
| [`thread_state_get`](#thread_state_get) | Read what this agent saved about the person it is talking to. | the message names its sender (`src_ilk`), and the node's state store opened at start |
| [`thread_state_put`](#thread_state_put) | Save what this agent needs to remember about the person it is talking to. | the message names its sender (`src_ilk`), and the node's state store opened at start |
| [`thread_state_delete`](#thread_state_delete) | Delete what this agent saved about the person it is talking to. | the message names its sender (`src_ilk`), and the node's state store opened at start |
| [`generate_csv_artifact`](#generate_csv_artifact) | Make a CSV file from rows of text and send it to the user. | always |
| [`generate_text_artifact`](#generate_text_artifact) | Make a plain-text file and send it to the user. | always |
| [`generate_json_artifact`](#generate_json_artifact) | Make a JSON file and send it to the user. | always |
| [`generate_markdown_artifact`](#generate_markdown_artifact) | Make a Markdown file and send it to the user. | always |
| [`generate_html_artifact`](#generate_html_artifact) | Make an HTML file and send it to the user. | always |
| [`generate_pdf_artifact`](#generate_pdf_artifact) | Make a one-page PDF of text lines and send it to the user. | always |
| [`generate_xlsx_artifact`](#generate_xlsx_artifact) | Make an XLSX spreadsheet of text cells and send it to the user. | always |
| [`generate_docx_artifact`](#generate_docx_artifact) | Make a DOCX document of plain paragraphs and send it to the user. | always |
| [`generate_png_artifact`](#generate_png_artifact) | Make a PNG image of colored horizontal bands and send it to the user. | always |
| [`generate_jpeg_artifact`](#generate_jpeg_artifact) | Make a JPEG image of colored horizontal bands and send it to the user. | always |
| [`publish_html_page`](#publish_html_page) | Publish an HTML page at a public link, and get the link. | the node has its identity: it was launched with its ILK |

## `thread_state_get`

Read what this agent saved about the person it is talking to.

- **When to use:** At the start of a turn that continues earlier work with this person: the step reached, the data already collected, a choice they made.
- **Present when:** the message names its sender (`src_ilk`), and the node's state store opened at start
- **Limits:**
  - One record per person: every call works on the record of whoever sent the current message, shared by all their conversations with this agent. `state_key` is required, but it does not choose the record.
  - Only this agent sees the record: it is kept on its node's disk.
- **Returns:** `{found: true, state_key, data, updated_at, ttl_seconds}` with the saved record, or `{found: false, state_key}` when there is none.
- **Errors:** no error code of its own.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "state_key": {
      "description": "Any non-empty label. The record is always the sender's.",
      "minLength": 1,
      "type": "string"
    }
  },
  "required": [
    "state_key"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{found: true, state_key, data, updated_at, ttl_seconds}` with the saved record, or `{found: false, state_key}` when there is none.",
  "properties": {
    "data": {
      "description": "What `thread_state_put` saved."
    },
    "found": {
      "type": "boolean"
    },
    "state_key": {
      "description": "The record's key: the sender's ILK.",
      "type": "string"
    },
    "ttl_seconds": {
      "description": "The ttl_seconds of the last put. Nothing expires the record.",
      "type": [
        "integer",
        "null"
      ]
    },
    "updated_at": {
      "description": "When it was saved (RFC 3339).",
      "type": "string"
    }
  },
  "required": [
    "found",
    "state_key"
  ],
  "type": "object"
}
```

## `thread_state_put`

Save what this agent needs to remember about the person it is talking to.

- **When to use:** As soon as something worth keeping changes (a step reached, data collected, a decision), before you answer.
- **Present when:** the message names its sender (`src_ilk`), and the node's state store opened at start
- **Limits:**
  - One record per person: every call works on the record of whoever sent the current message, shared by all their conversations with this agent. `state_key` is required, but it does not choose the record.
  - A put replaces the whole record: keep everything in one object. Read it with `thread_state_get`, change it, and put all of it back.
  - Only this agent sees the record: it is kept on its node's disk.
  - `ttl_seconds` is stored with the record, but nothing expires it.
- **Returns:** `{ok: true, state_key}` once the record is saved.
- **Errors:** no error code of its own.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "data": {
      "description": "What to keep, as one JSON value: it replaces the record."
    },
    "state_key": {
      "description": "Any non-empty label. The record is always the sender's.",
      "minLength": 1,
      "type": "string"
    },
    "ttl_seconds": {
      "description": "Kept with the record and returned by `thread_state_get`; nothing expires it.",
      "minimum": 1,
      "type": "integer"
    }
  },
  "required": [
    "state_key",
    "data"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{ok: true, state_key}` once the record is saved.",
  "properties": {
    "ok": {
      "const": true
    },
    "state_key": {
      "description": "The record's key: the sender's ILK.",
      "type": "string"
    }
  },
  "required": [
    "ok",
    "state_key"
  ],
  "type": "object"
}
```

## `thread_state_delete`

Delete what this agent saved about the person it is talking to.

- **When to use:** When the work with this person is over and nothing should carry into their next conversation, or when they ask you to forget it.
- **Present when:** the message names its sender (`src_ilk`), and the node's state store opened at start
- **Limits:**
  - It deletes the record of whoever sent the current message, whatever `state_key` says.
  - Deleting a record that does not exist succeeds.
- **Returns:** `{ok: true, state_key}` once the record is gone.
- **Errors:** no error code of its own.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "state_key": {
      "description": "Any non-empty label. The record is always the sender's.",
      "minLength": 1,
      "type": "string"
    }
  },
  "required": [
    "state_key"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{ok: true, state_key}` once the record is gone.",
  "properties": {
    "ok": {
      "const": true
    },
    "state_key": {
      "description": "The record's key: the sender's ILK.",
      "type": "string"
    }
  },
  "required": [
    "ok",
    "state_key"
  ],
  "type": "object"
}
```

## `generate_csv_artifact`

Make a CSV file from rows of text and send it to the user.

- **When to use:** When the user wants a table as a file, to open in a spreadsheet or another program.
- **Present when:** always
- **Limits:**
  - Every cell is written quoted. A cell that starts with `=`, `+`, `@`, a tab or a carriage return, or with `-` and is not a plain number such as `-12.5`, gets a `'` in front, so a spreadsheet takes it as text, never as a formula.
  - `headers`, when given, is the first row.
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .csv. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .csv, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "headers": {
      "description": "The first row.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "rows": {
      "description": "The rows, each a list of cells.",
      "items": {
        "items": {
          "type": "string"
        },
        "type": "array"
      },
      "type": "array"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "rows"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "text/csv"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_text_artifact`

Make a plain-text file and send it to the user.

- **When to use:** When the user wants text as a file rather than in the message: notes, a log, a long answer to keep.
- **Present when:** always
- **Limits:**
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .txt. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "content": {
      "description": "The file's text.",
      "type": "string"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .txt, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "content"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "text/plain"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_json_artifact`

Make a JSON file and send it to the user.

- **When to use:** When the user wants structured data as a file, for a program to read.
- **Present when:** always
- **Limits:**
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .json. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "data": {
      "description": "Any JSON value: it is the file's content."
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .json, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "pretty": {
      "description": "Indent the file; true by default.",
      "type": "boolean"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "data"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "application/json"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_markdown_artifact`

Make a Markdown file and send it to the user.

- **When to use:** When the user wants formatted text (headings, lists, tables) as a file.
- **Present when:** always
- **Limits:**
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .md or .markdown. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "content": {
      "description": "The file's Markdown.",
      "type": "string"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .md or .markdown, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "content"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "text/markdown"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_html_artifact`

Make an HTML file and send it to the user.

- **When to use:** When the user wants a page as a file, to open in a browser. For a link to a page instead, use `publish_html_page` when you have it.
- **Present when:** always
- **Limits:**
  - It is a file, not a link: the user opens it from the attachment.
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .html or .htm. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "content": {
      "description": "The whole HTML document.",
      "type": "string"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .html or .htm, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "content"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "text/html"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_pdf_artifact`

Make a one-page PDF of text lines and send it to the user.

- **When to use:** When the user wants a short text document as a PDF.
- **Present when:** always
- **Limits:**
  - One page (US Letter) of Helvetica 12 pt, the title first and then one line per entry of `lines`. Lines do not wrap (about 90 characters fit) and nothing goes past the page (about 50 lines fit). No images, tables or styles.
  - Only plain ASCII prints right: accented letters and other non-ASCII characters come out garbled.
  - With no title and no lines, the page says "Documento generado por AI".
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .pdf. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .pdf, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "lines": {
      "description": "The page's lines, top to bottom.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    },
    "title": {
      "description": "The first line, followed by a blank one.",
      "type": "string"
    }
  },
  "required": [
    "filename"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "application/pdf"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_xlsx_artifact`

Make an XLSX spreadsheet of text cells and send it to the user.

- **When to use:** When the user wants a table as an Excel file.
- **Present when:** always
- **Limits:**
  - One sheet, and every cell is text: numbers stay text, with no formulas or formats. `headers`, when given, is the first row.
  - `sheet_name` loses `:`, `\`, `/`, `?`, `*`, `[` and `]` and is cut at 31 characters; it is Sheet1 without one.
  - With no headers and no rows, the sheet holds one cell: "Generated by AI".
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .xlsx. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .xlsx, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "headers": {
      "description": "The first row.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "rows": {
      "description": "The rows, each a list of cells.",
      "items": {
        "items": {
          "type": "string"
        },
        "type": "array"
      },
      "type": "array"
    },
    "sheet_name": {
      "description": "Sheet1 by default.",
      "type": "string"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    }
  },
  "required": [
    "filename",
    "rows"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_docx_artifact`

Make a DOCX document of plain paragraphs and send it to the user.

- **When to use:** When the user wants a text document to edit in Word.
- **Present when:** always
- **Limits:**
  - Plain text in a fixed order: the title, the paragraphs, the bullets, then one table. No styles or headings: the title is a plain paragraph, and a bullet is a paragraph that starts with "- ".
  - With nothing to write, the document says "Documento generado por AI".
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .docx. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "bullets": {
      "description": "One \"- \" line per entry, after the paragraphs.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .docx, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "paragraphs": {
      "description": "One paragraph per entry.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "table_headers": {
      "description": "The table's first row.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "table_rows": {
      "description": "The table's rows, each a list of cells.",
      "items": {
        "items": {
          "type": "string"
        },
        "type": "array"
      },
      "type": "array"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    },
    "title": {
      "description": "The first paragraph.",
      "type": "string"
    }
  },
  "required": [
    "filename"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_png_artifact`

Make a PNG image of colored horizontal bands and send it to the user.

- **When to use:** Only when an image of colored bands will do, such as a placeholder: it cannot draw text, charts or pictures.
- **Present when:** always
- **Limits:**
  - It draws only horizontal bands of color, one per entry of `bands`, top to bottom: each entry's text picks its color and is not drawn. Without `bands`, one color fills the image.
  - 1200 x 800 pixels by default; `width` and `height` are kept between 64 and 2400.
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .png. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "bands": {
      "description": "One horizontal band per entry, top to bottom.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .png, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "height": {
      "description": "Pixels; 800 by default.",
      "maximum": 2400,
      "minimum": 64,
      "type": "integer"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    },
    "width": {
      "description": "Pixels; 1200 by default.",
      "maximum": 2400,
      "minimum": 64,
      "type": "integer"
    }
  },
  "required": [
    "filename"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "image/png"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `generate_jpeg_artifact`

Make a JPEG image of colored horizontal bands and send it to the user.

- **When to use:** Only when an image of colored bands will do, such as a placeholder: it cannot draw text, charts or pictures.
- **Present when:** always
- **Limits:**
  - It draws only horizontal bands of color, one per entry of `bands`, top to bottom: each entry's text picks its color and is not drawn. Without `bands`, one color fills the image.
  - 1200 x 800 pixels by default; `width` and `height` are kept between 64 and 2400.
  - The file goes out with this call's `text` as your reply: what you write after the call is not sent. One file per reply: if several calls make files, only the last is sent. The file's bytes come back to you base64-encoded, so a large file fills your context.
- **Returns:** `{status: "ok", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .jpg or .jpeg. No file was made.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "bands": {
      "description": "One horizontal band per entry, top to bottom.",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "filename": {
      "description": "The file's name, as the user sees it: it ends in .jpg or .jpeg, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    },
    "height": {
      "description": "Pixels; 800 by default.",
      "maximum": 2400,
      "minimum": 64,
      "type": "integer"
    },
    "text": {
      "description": "Your reply, which the user reads with the file. Without it, a fixed line in Spanish naming the file.",
      "type": "string"
    },
    "width": {
      "description": "Pixels; 1200 by default.",
      "maximum": 2400,
      "minimum": 64,
      "type": "integer"
    }
  },
  "required": [
    "filename"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", final_output: {text, artifacts: [{filename, mime, bytes_base64}]}}`: the file and the reply's text.",
  "oneOf": [
    {
      "properties": {
        "final_output": {
          "properties": {
            "artifacts": {
              "items": {
                "properties": {
                  "bytes_base64": {
                    "description": "The file, base64-encoded.",
                    "type": "string"
                  },
                  "filename": {
                    "type": "string"
                  },
                  "mime": {
                    "const": "image/jpeg"
                  }
                },
                "required": [
                  "filename",
                  "mime",
                  "bytes_base64"
                ],
                "type": "object"
              },
              "maxItems": 1,
              "minItems": 1,
              "type": "array"
            },
            "text": {
              "description": "The reply's text.",
              "type": "string"
            }
          },
          "required": [
            "text",
            "artifacts"
          ],
          "type": "object"
        },
        "status": {
          "const": "ok"
        }
      },
      "required": [
        "status",
        "final_output"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

## `publish_html_page`

Publish an HTML page at a public link, and get the link.

- **When to use:** Only when the user asks for a page to open or share by link. To hand over a file instead, use `generate_html_artifact`.
- **Present when:** the node has its identity: it was launched with its ILK
- **Limits:**
  - One self-contained HTML file, served under a sandbox: its inline scripts and styles run, but it loads nothing from the network (no fetch, no outside scripts, stylesheets, fonts or images); images and fonts only inline, as `data:` URLs; forms do not submit.
  - The link is the key: anyone who has it can open the page, so put nothing private in it. It expires 24 hours after publishing, and you cannot extend it or take the page down earlier.
  - Publishing takes up to two minutes. Give the link in your reply; if a `generate_*_artifact` call also makes a file for the same reply, only that call's `text` is sent, so put the link there.
- **Returns:** `{status: "ok", url, publication_id, expires_at}`: the public link, the publication's id and when it expires (Unix seconds).
- **Errors:**
  - `artifact_filename_invalid`: `filename` is empty, longer than 128 bytes, has `/`, `\`, `..` or a control character, or does not end in .html or .htm. Nothing was published.
  - `publish_failed`: The page was not published; `message` says why. `retryable: true` when this node's blob store or SY.admin could not be reached; `false` when SY.admin refused it (the message carries its code, such as BLOB_TOO_LARGE) or the hive has no public address for links.

Input:

```json
{
  "additionalProperties": false,
  "properties": {
    "content": {
      "description": "The whole HTML document, self-contained.",
      "type": "string"
    },
    "filename": {
      "description": "The page's file name: it ends in .html or .htm, with at most 128 bytes and no `/`, `\\`, `..` or control character.",
      "minLength": 1,
      "type": "string"
    }
  },
  "required": [
    "filename",
    "content"
  ],
  "type": "object"
}
```

Output:

```json
{
  "description": "`{status: \"ok\", url, publication_id, expires_at}`: the public link, the publication's id and when it expires (Unix seconds).",
  "oneOf": [
    {
      "properties": {
        "expires_at": {
          "description": "Unix seconds.",
          "type": "integer"
        },
        "note": {
          "type": "string"
        },
        "publication_id": {
          "type": "string"
        },
        "status": {
          "const": "ok"
        },
        "url": {
          "description": "The public link.",
          "type": "string"
        }
      },
      "required": [
        "status",
        "url"
      ],
      "type": "object"
    },
    {
      "properties": {
        "error_code": {
          "enum": [
            "artifact_filename_invalid",
            "publish_failed"
          ]
        },
        "message": {
          "type": "string"
        },
        "retryable": {
          "type": "boolean"
        },
        "status": {
          "const": "error"
        }
      },
      "required": [
        "status",
        "error_code",
        "message",
        "retryable"
      ],
      "type": "object"
    }
  ]
}
```

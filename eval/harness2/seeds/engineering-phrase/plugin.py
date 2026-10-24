"""Fill each field with the text after its cue phrase, up to the next period, in the first `hits` memories."""
CUES = {"owner": "owner is ", "rollback_image": "restore image ", "root_cause": "root cause is ",
        "launch_date": "launch is "}


def run(task):
    out = {}
    for item in task["items"]:
        x = item["input"]
        record = {}
        for field in x["fields"]:
            record[field] = ""
            for hit in x["hits"][:task["params"]["hits"]]:
                text = hit["text"]
                start = text.find(CUES[field])
                if start >= 0:
                    record[field] = text[start + len(CUES[field]):].split(".")[0].strip()
                    break
        out[item["id"]] = record
    return out

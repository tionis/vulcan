#!/usr/bin/env python3
import os
import random
import time
from concurrent.futures import ProcessPoolExecutor
from datetime import date, timedelta

NUM_NOTES = 10000
LINKS_PER_NOTE = 10
OUTPUT_DIR = "synthetic_vault"
FOLDERS = 100  # Distribute files into folders to avoid filesystem limits

# Base block of text to repeat. This is ~445 bytes.
LOREM_IPSUM = """Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."""

STATUSES = ["todo", "in-progress", "done"]
TAGS = [
    "project", "research", "reference", "inbox", "archive",
    "meeting", "idea", "review", "draft", "published",
]

# Generate all dates in 2024 once
START_DATE = date(2024, 1, 1)
ALL_DATES = [START_DATE + timedelta(days=i) for i in range(366)]  # 2024 is a leap year


def get_path_for_note(note_id):
    """Determine the file path for a given note ID."""
    folder_id = note_id % FOLDERS
    folder_name = f"Folder_{folder_id:03d}"
    filename = f"Note_{note_id:06d}.md"
    return os.path.join(OUTPUT_DIR, folder_name, filename)


def generate_note(args):
    """Generate the content for a single note and write it to disk."""
    note_id, total_notes = args

    file_path = get_path_for_note(note_id)

    # Use a per-note seed so output is deterministic but varied.
    rng = random.Random(note_id)

    # Frontmatter
    title = f"Note {note_id:06d}"
    status = rng.choice(STATUSES)
    priority = rng.randint(1, 3)
    note_date = rng.choice(ALL_DATES).isoformat()
    note_tags = rng.sample(TAGS, k=rng.randint(1, 2))
    tags_yaml = ", ".join(f'"{t}"' for t in note_tags)

    frontmatter = (
        "---\n"
        f'title: "{title}"\n'
        f"status: {status}\n"
        f"priority: {priority}\n"
        f"date: {note_date}\n"
        f"tags: [{tags_yaml}]\n"
        "---\n"
    )

    num_paragraphs = rng.randint(5, 8)
    paragraphs = [LOREM_IPSUM] * num_paragraphs

    # Insert random interlinks
    for _ in range(LINKS_PER_NOTE):
        target_id = rng.randint(0, total_notes - 1)
        target_name = f"Note_{target_id:06d}"

        # Pick a random paragraph to append the link to
        p_idx = rng.randint(0, len(paragraphs) - 1)
        paragraphs[p_idx] += f" [[{target_name}]]"

    content = frontmatter + f"\n# Note_{note_id:06d}\n\n" + "\n\n".join(paragraphs)

    with open(file_path, "w", encoding="utf-8") as f:
        f.write(content)


def main():
    print(f"Setting up directories in '{OUTPUT_DIR}'...")
    for i in range(FOLDERS):
        folder_path = os.path.join(OUTPUT_DIR, f"Folder_{i:03d}")
        os.makedirs(folder_path, exist_ok=True)

    print(f"Generating {NUM_NOTES} notes with ~{LINKS_PER_NOTE} links each...")
    start_time = time.time()

    # Using ProcessPoolExecutor to heavily parallelize file generation
    chunk_size = 500
    args_iter = ((i, NUM_NOTES) for i in range(NUM_NOTES))

    completed = 0
    with ProcessPoolExecutor() as executor:
        for _ in executor.map(generate_note, args_iter, chunksize=chunk_size):
            completed += 1
            if completed % 2000 == 0:
                print(f"Generated {completed} / {NUM_NOTES} notes")

    elapsed = time.time() - start_time
    print(f"Done! Generated {NUM_NOTES} files in {elapsed:.2f} seconds.")


if __name__ == "__main__":
    main()

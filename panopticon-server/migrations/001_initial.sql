-- Panopticon initial schema

-- Repositories table
CREATE TABLE repos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL UNIQUE,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    last_commit_sha TEXT,
    last_checked_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    UNIQUE(owner, name)
);

CREATE INDEX idx_repos_owner_name ON repos(owner, name);
CREATE INDEX idx_repos_last_checked_at ON repos(last_checked_at);

-- Prompts table (multiple per repo)
CREATE TABLE prompts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    text TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    UNIQUE(repo_id, name)
);

CREATE INDEX idx_prompts_repo_id ON prompts(repo_id);

-- Reviews table
CREATE TABLE reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    commit_sha TEXT NOT NULL,
    trigger TEXT NOT NULL CHECK (trigger IN ('scheduled', 'manual')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'in_progress', 'completed', 'failed')),
    started_at TEXT,
    completed_at TEXT,
    duration_secs INTEGER,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_reviews_repo_id ON reviews(repo_id);
CREATE INDEX idx_reviews_status ON reviews(status);
CREATE INDEX idx_reviews_created_at ON reviews(created_at DESC);

-- Review results (one per prompt per review)
CREATE TABLE review_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    prompt_id INTEGER NOT NULL REFERENCES prompts(id) ON DELETE CASCADE,
    prompt_name TEXT NOT NULL,
    detailed_reasoning TEXT NOT NULL,
    action_required INTEGER NOT NULL,
    user_visible_comments TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    UNIQUE(review_id, prompt_id)
);

CREATE INDEX idx_review_results_review_id ON review_results(review_id);

-- Job queue table (persistent, survives restarts)
CREATE TABLE jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    scheduled_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_jobs_status_scheduled ON jobs(status, scheduled_at);

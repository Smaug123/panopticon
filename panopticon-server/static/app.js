// Panopticon Frontend - Pure JS, no dependencies

// --- State ---
const state = {
    repos: [],
    currentRepo: null,
    currentReview: null,
    apiKey: localStorage.getItem('panopticon_api_key') || '',
};

// --- API Client ---

async function api(method, path, body = null) {
    if (!state.apiKey) {
        promptForApiKey();
        throw new Error('API key required');
    }

    const headers = {
        'Authorization': `Bearer ${state.apiKey}`,
        'Content-Type': 'application/json',
    };

    const options = { method, headers };
    if (body) {
        options.body = JSON.stringify(body);
    }

    const response = await fetch(`/api${path}`, options);

    if (response.status === 401) {
        localStorage.removeItem('panopticon_api_key');
        state.apiKey = '';
        promptForApiKey();
        throw new Error('Unauthorized');
    }

    if (!response.ok) {
        const error = await response.json().catch(() => ({ error: 'Unknown error' }));
        throw new Error(error.error || 'Request failed');
    }

    if (response.status === 204) {
        return null;
    }

    return response.json();
}

// --- Utilities ---

function formatDate(dateString) {
    const date = new Date(dateString);
    return date.toLocaleDateString('en-US', {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
    });
}

function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// Simple markdown to HTML conversion with XSS protection.
// IMPORTANT: Content from LLM is untrusted and must be escaped before rendering.
function renderMarkdown(text) {
    if (!text) return '';

    // SECURITY: First escape all HTML to prevent XSS from LLM output
    const escaped = escapeHtml(text);

    return escaped
        // Code blocks - use escaped content
        .replace(/```(\w*)\n([\s\S]*?)```/g, '<pre><code>$2</code></pre>')
        // Inline code
        .replace(/`([^`]+)`/g, '<code>$1</code>')
        // Headers
        .replace(/^#### (.+)$/gm, '<h4>$1</h4>')
        .replace(/^### (.+)$/gm, '<h3>$1</h3>')
        .replace(/^## (.+)$/gm, '<h2>$1</h2>')
        .replace(/^# (.+)$/gm, '<h1>$1</h1>')
        // Bold
        .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
        // Italic
        .replace(/\*([^*]+)\*/g, '<em>$1</em>')
        // Lists
        .replace(/^\s*[-*] (.+)$/gm, '<li>$1</li>')
        .replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>')
        // Paragraphs
        .replace(/\n\n/g, '</p><p>')
        .replace(/^(.+)$/gm, function(match) {
            if (match.startsWith('<')) return match;
            return `<p>${match}</p>`;
        });
}

// --- Modal ---

function showModal(content) {
    const overlay = document.getElementById('modal-overlay');
    const modalContent = document.getElementById('modal-content');
    modalContent.innerHTML = content;
    overlay.classList.remove('hidden');
}

function hideModal() {
    document.getElementById('modal-overlay').classList.add('hidden');
}

function promptForApiKey() {
    showModal(`
        <div class="modal-header">
            <h3>API Key Required</h3>
        </div>
        <form id="api-key-form">
            <div class="form-group">
                <label for="api-key-input">Enter your API key</label>
                <input type="password" id="api-key-input" required>
            </div>
            <div class="form-actions">
                <button type="submit" class="btn btn-primary">Save</button>
            </div>
        </form>
    `);

    document.getElementById('api-key-form').addEventListener('submit', (e) => {
        e.preventDefault();
        const key = document.getElementById('api-key-input').value;
        state.apiKey = key;
        localStorage.setItem('panopticon_api_key', key);
        hideModal();
        showReposList();
    });
}

// --- Repos List ---

async function loadRepos() {
    try {
        state.repos = await api('GET', '/repos');
        renderReposList();
    } catch (e) {
        console.error('Failed to load repos:', e);
    }
}

function renderReposList() {
    const container = document.getElementById('repos-list');

    if (state.repos.length === 0) {
        container.innerHTML = `
            <div class="empty-state">
                <p>No repositories registered yet.</p>
                <button class="btn btn-primary" onclick="showAddRepoModal()">Add Your First Repository</button>
            </div>
        `;
        return;
    }

    container.innerHTML = state.repos.map(repo => `
        <div class="card" onclick="showRepoDetail(${repo.id})">
            <div class="card-header">
                <h3>${escapeHtml(repo.owner)}/${escapeHtml(repo.name)}</h3>
                <span class="status-badge ${repo.last_review?.status || 'none'}">
                    ${repo.last_review?.status?.replace('_', ' ') || 'No reviews'}
                </span>
            </div>
            <div class="card-body">
                <p>${repo.prompt_count} prompt(s)</p>
                ${repo.last_review ? `<p>Last review: ${formatDate(repo.last_review.created_at)}</p>` : ''}
            </div>
        </div>
    `).join('');
}

function showAddRepoModal() {
    showModal(`
        <div class="modal-header">
            <h3>Add Repository</h3>
            <button class="modal-close" onclick="hideModal()">&times;</button>
        </div>
        <form id="add-repo-form">
            <div class="form-group">
                <label for="repo-url">GitHub Repository URL</label>
                <input type="url" id="repo-url" placeholder="https://github.com/owner/repo" required>
            </div>
            <div class="form-actions">
                <button type="button" class="btn btn-secondary" onclick="hideModal()">Cancel</button>
                <button type="submit" class="btn btn-primary">Add Repository</button>
            </div>
        </form>
    `);

    document.getElementById('add-repo-form').addEventListener('submit', async (e) => {
        e.preventDefault();
        const url = document.getElementById('repo-url').value;
        try {
            await api('POST', '/repos', { url });
            hideModal();
            loadRepos();
        } catch (e) {
            alert('Failed to add repository: ' + e.message);
        }
    });
}

// --- Repo Detail ---

async function showRepoDetail(repoId) {
    try {
        state.currentRepo = await api('GET', `/repos/${repoId}`);

        document.getElementById('repos-section').classList.add('hidden');
        document.getElementById('repo-detail-section').classList.remove('hidden');
        document.getElementById('review-detail-section').classList.add('hidden');

        renderRepoHeader();
        loadPrompts();
        loadReviews();
    } catch (e) {
        console.error('Failed to load repo:', e);
        alert('Failed to load repository: ' + e.message);
    }
}

function renderRepoHeader() {
    const repo = state.currentRepo;
    document.getElementById('repo-header').innerHTML = `
        <h2>${escapeHtml(repo.owner)}/${escapeHtml(repo.name)}</h2>
        <p class="text-muted text-sm">${escapeHtml(repo.url)}</p>
        ${repo.last_commit_sha ? `<p class="text-muted text-sm">Last commit: ${repo.last_commit_sha.substring(0, 7)}</p>` : ''}
    `;
}

// --- Prompts ---

async function loadPrompts() {
    try {
        const prompts = await api('GET', `/repos/${state.currentRepo.id}/prompts`);
        renderPromptsList(prompts);
    } catch (e) {
        console.error('Failed to load prompts:', e);
    }
}

function renderPromptsList(prompts) {
    const container = document.getElementById('prompts-list');

    if (prompts.length === 0) {
        container.innerHTML = '<div class="empty-state"><p>No prompts configured.</p></div>';
        return;
    }

    container.innerHTML = prompts.map(prompt => `
        <div class="prompt-item">
            <div class="prompt-item-header">
                <h4>
                    ${escapeHtml(prompt.name)}
                    ${prompt.is_default ? '<span class="badge badge-default">Default</span>' : ''}
                    ${!prompt.enabled ? '<span class="badge badge-disabled">Disabled</span>' : ''}
                </h4>
                <div class="prompt-item-actions">
                    <button class="btn btn-secondary" onclick="showEditPromptModal(${prompt.id})">Edit</button>
                    ${!prompt.is_default ? `<button class="btn btn-danger" onclick="deletePrompt(${prompt.id})">Delete</button>` : ''}
                </div>
            </div>
            <div class="prompt-text">${escapeHtml(prompt.text.substring(0, 200))}${prompt.text.length > 200 ? '...' : ''}</div>
        </div>
    `).join('');
}

function showAddPromptModal() {
    showModal(`
        <div class="modal-header">
            <h3>Add Prompt</h3>
            <button class="modal-close" onclick="hideModal()">&times;</button>
        </div>
        <form id="add-prompt-form">
            <div class="form-group">
                <label for="prompt-name">Name</label>
                <input type="text" id="prompt-name" required>
            </div>
            <div class="form-group">
                <label for="prompt-text">Prompt Text</label>
                <textarea id="prompt-text" required placeholder="Describe what you want the LLM to focus on..."></textarea>
            </div>
            <div class="form-actions">
                <button type="button" class="btn btn-secondary" onclick="hideModal()">Cancel</button>
                <button type="submit" class="btn btn-primary">Add Prompt</button>
            </div>
        </form>
    `);

    document.getElementById('add-prompt-form').addEventListener('submit', async (e) => {
        e.preventDefault();
        const name = document.getElementById('prompt-name').value;
        const text = document.getElementById('prompt-text').value;
        try {
            await api('POST', `/repos/${state.currentRepo.id}/prompts`, { name, text });
            hideModal();
            loadPrompts();
        } catch (e) {
            alert('Failed to add prompt: ' + e.message);
        }
    });
}

async function showEditPromptModal(promptId) {
    const prompts = await api('GET', `/repos/${state.currentRepo.id}/prompts`);
    const prompt = prompts.find(p => p.id === promptId);
    if (!prompt) return;

    showModal(`
        <div class="modal-header">
            <h3>Edit Prompt</h3>
            <button class="modal-close" onclick="hideModal()">&times;</button>
        </div>
        <form id="edit-prompt-form">
            <div class="form-group">
                <label for="prompt-name">Name</label>
                <input type="text" id="prompt-name" value="${escapeHtml(prompt.name)}" required>
            </div>
            <div class="form-group">
                <label for="prompt-text">Prompt Text</label>
                <textarea id="prompt-text" required>${escapeHtml(prompt.text)}</textarea>
            </div>
            <div class="form-group checkbox-group">
                <input type="checkbox" id="prompt-enabled" ${prompt.enabled ? 'checked' : ''}>
                <label for="prompt-enabled">Enabled</label>
            </div>
            <div class="form-actions">
                <button type="button" class="btn btn-secondary" onclick="hideModal()">Cancel</button>
                <button type="submit" class="btn btn-primary">Save Changes</button>
            </div>
        </form>
    `);

    document.getElementById('edit-prompt-form').addEventListener('submit', async (e) => {
        e.preventDefault();
        const name = document.getElementById('prompt-name').value;
        const text = document.getElementById('prompt-text').value;
        const enabled = document.getElementById('prompt-enabled').checked;
        try {
            await api('PUT', `/repos/${state.currentRepo.id}/prompts/${promptId}`, { name, text, enabled });
            hideModal();
            loadPrompts();
        } catch (e) {
            alert('Failed to update prompt: ' + e.message);
        }
    });
}

async function deletePrompt(promptId) {
    if (!confirm('Are you sure you want to delete this prompt?')) return;
    try {
        await api('DELETE', `/repos/${state.currentRepo.id}/prompts/${promptId}`);
        loadPrompts();
    } catch (e) {
        alert('Failed to delete prompt: ' + e.message);
    }
}

// --- Reviews ---

async function loadReviews() {
    try {
        const reviews = await api('GET', `/repos/${state.currentRepo.id}/reviews`);
        renderReviewsList(reviews);
    } catch (e) {
        console.error('Failed to load reviews:', e);
    }
}

function renderReviewsList(reviews) {
    const container = document.getElementById('reviews-list');

    if (reviews.length === 0) {
        container.innerHTML = `
            <div class="empty-state">
                <p>No reviews yet.</p>
                <button class="btn btn-primary" onclick="triggerReview()">Trigger First Review</button>
            </div>
        `;
        return;
    }

    container.innerHTML = reviews.map(review => `
        <div class="review-item" onclick="showReviewDetail(${review.id})">
            <div class="review-item-info">
                <span class="status-badge ${review.status.status}">${review.status.status.replace('_', ' ')}</span>
                <span class="review-item-meta">
                    ${review.trigger} &bull; ${formatDate(review.created_at)} &bull; ${review.commit_sha.substring(0, 7)}
                </span>
            </div>
        </div>
    `).join('');
}

async function triggerReview() {
    try {
        await api('POST', `/repos/${state.currentRepo.id}/reviews`);
        alert('Review scheduled. Refresh to see progress.');
        loadReviews();
    } catch (e) {
        alert('Failed to trigger review: ' + e.message);
    }
}

// --- Review Detail ---

async function showReviewDetail(reviewId) {
    try {
        state.currentReview = await api('GET', `/reviews/${reviewId}`);

        document.getElementById('repos-section').classList.add('hidden');
        document.getElementById('repo-detail-section').classList.add('hidden');
        document.getElementById('review-detail-section').classList.remove('hidden');

        renderReviewDetail();

        // If in progress, start streaming
        if (state.currentReview.status.status === 'in_progress') {
            startReviewStream(reviewId);
        }
    } catch (e) {
        console.error('Failed to load review:', e);
        alert('Failed to load review: ' + e.message);
    }
}

function renderReviewDetail() {
    const review = state.currentReview;

    document.getElementById('review-header').innerHTML = `
        <div class="section-header">
            <h2>Review Details</h2>
            <span class="status-badge ${review.status.status}">${review.status.status.replace('_', ' ')}</span>
        </div>
        <p class="text-muted text-sm">
            Trigger: ${review.trigger} &bull;
            Commit: ${review.commit_sha.substring(0, 7)} &bull;
            Created: ${formatDate(review.created_at)}
        </p>
    `;

    const content = document.getElementById('review-content');

    if (review.status.status === 'in_progress') {
        content.innerHTML = `
            <div class="streaming-indicator">
                <span class="pulse"></span>
                <span>Review in progress...</span>
            </div>
            <div id="streaming-content"></div>
        `;
        return;
    }

    if (review.status.status === 'failed') {
        content.innerHTML = `
            <div class="review-result">
                <div class="review-result-header">
                    <h4>Error</h4>
                </div>
                <div class="review-result-body">
                    <p class="text-muted">${escapeHtml(review.status.error || 'Unknown error')}</p>
                </div>
            </div>
        `;
        return;
    }

    if (review.results.length === 0) {
        content.innerHTML = '<div class="empty-state"><p>No results available.</p></div>';
        return;
    }

    content.innerHTML = review.results.map(result => `
        <div class="review-result">
            <div class="review-result-header">
                <h4>${escapeHtml(result.prompt_name)}</h4>
                ${result.action_required ? '<span class="action-required">Action Required</span>' : ''}
            </div>
            <div class="review-result-body markdown-content">
                ${renderMarkdown(result.user_visible_comments)}
            </div>
        </div>
    `).join('');
}

function startReviewStream(reviewId) {
    const eventSource = new EventSource(`/api/reviews/${reviewId}/stream`);
    let currentPrompt = '';
    let content = {};

    eventSource.onmessage = (event) => {
        const data = JSON.parse(event.data);
        const streamingContent = document.getElementById('streaming-content');
        if (!streamingContent) return;

        if (data.type === 'chunk') {
            if (!content[data.prompt_name]) {
                content[data.prompt_name] = '';
            }
            content[data.prompt_name] += data.text;

            // Re-render all content
            streamingContent.innerHTML = Object.entries(content).map(([name, text]) => `
                <div class="review-result">
                    <div class="review-result-header">
                        <h4>${escapeHtml(name)}</h4>
                    </div>
                    <div class="review-result-body">
                        <pre style="white-space: pre-wrap; font-size: 0.8rem;">${escapeHtml(text)}</pre>
                    </div>
                </div>
            `).join('');
        } else if (data.type === 'prompt_complete') {
            // A single prompt finished, but there may be more prompts.
            // Keep the stream open and continue receiving updates.
        } else if (data.type === 'complete') {
            // All prompts completed - refresh to get final results
            eventSource.close();
            showReviewDetail(reviewId);
        }
    };

    eventSource.onerror = () => {
        eventSource.close();
        // Refresh to get current state
        showReviewDetail(reviewId);
    };
}

// --- Navigation ---

function showReposList() {
    document.getElementById('repos-section').classList.remove('hidden');
    document.getElementById('repo-detail-section').classList.add('hidden');
    document.getElementById('review-detail-section').classList.add('hidden');
    loadRepos();
}

function backToRepoDetail() {
    document.getElementById('repos-section').classList.add('hidden');
    document.getElementById('repo-detail-section').classList.remove('hidden');
    document.getElementById('review-detail-section').classList.add('hidden');
}

// --- Tabs ---

function switchTab(tabName) {
    document.querySelectorAll('.tab').forEach(tab => {
        tab.classList.toggle('active', tab.dataset.tab === tabName);
    });

    document.querySelectorAll('.tab-panel').forEach(panel => {
        panel.classList.toggle('hidden', panel.id !== `${tabName}-panel`);
    });
}

// --- Event Listeners ---

document.addEventListener('DOMContentLoaded', () => {
    if (!state.apiKey) {
        promptForApiKey();
    } else {
        showReposList();
    }

    // Navigation
    document.getElementById('back-btn').addEventListener('click', showReposList);
    document.getElementById('review-back-btn').addEventListener('click', backToRepoDetail);

    // Buttons
    document.getElementById('add-repo-btn').addEventListener('click', showAddRepoModal);
    document.getElementById('add-prompt-btn').addEventListener('click', showAddPromptModal);
    document.getElementById('trigger-review-btn').addEventListener('click', triggerReview);

    // Tabs
    document.querySelectorAll('.tab').forEach(tab => {
        tab.addEventListener('click', () => switchTab(tab.dataset.tab));
    });

    // Modal close on overlay click
    document.getElementById('modal-overlay').addEventListener('click', (e) => {
        if (e.target === e.currentTarget) {
            hideModal();
        }
    });

    // Escape key closes modal
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') {
            hideModal();
        }
    });
});

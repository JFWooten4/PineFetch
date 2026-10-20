import { formatHistorySource } from './formatters.js';
import { createHistorySocialInsightsView } from './history-social-insights-view.js';

export const createHistoryAccountView = ({ els, api, appendLog, openVideo, back, prepareDownload }) => {
    const state = {
        id: null,
        request: 0,
        offset: 0,
        reading: false,
        profile: '',
        remoteOffset: 0,
        remoteMore: false,
        remoteBusy: false,
    };
    const pendingVideos = new Set();
    const socialEls = Object.fromEntries(
        ['LoadBtn', 'Results', 'Status', 'Meta', 'Metrics'].map(key => [
            `historySocial${key}`,
            els[`historyAccountSocial${key}`],
        ])
    );
    const setProfile = url => {
        state.profile = url || '';
        els.historyAccountOpenBtn.hidden = !state.profile;
    };
    const social = createHistorySocialInsightsView({
        els: socialEls,
        profileMetrics: true,
        appendLog,
        api: {
            available: api.available,
            getHistorySocialInsights: async ({ id }) => (await api.getHistoryAccount({ id, offset: 0 })).insights,
            loadHistorySocialInsights: async ({ id }) => {
                const snapshot = await api.loadHistoryAccountInsights({ id });
                if (state.id === id) setProfile(snapshot.profile_url);
                return snapshot;
            },
        },
    });
    const savedRow = entry => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'oj-panel oj-panel-compact oj-panel-interactive pinefetch-account-video';
        const title = document.createElement('strong');
        title.textContent = entry.title || entry.filename || entry.url;
        const meta = document.createElement('span');
        meta.className = 'oj-helper';
        meta.textContent = [entry.medium, entry.filename].filter(Boolean).join(' · ');
        button.append(title, meta);
        button.addEventListener('click', () => openVideo(entry.id, button));
        return button;
    };
    const loadSaved = async (append = false) => {
        if (!state.id || state.reading || !api.available) return;
        const id = state.id;
        const request = state.request;
        state.reading = true;
        els.historyAccountMoreSavedBtn.disabled = true;
        els.historyAccountSavedList.setAttribute('aria-busy', 'true');
        els.historyAccountStatus.textContent = 'Loading account…';
        try {
            const page = await api.getHistoryAccount({ id, offset: append ? state.offset : 0 });
            if (state.id !== id || state.request !== request) return;
            els.historyAccountTitle.textContent = page.uploader;
            els.historyAccountMeta.textContent = formatHistorySource(page.source);
            els.historyAccountSavedCount.textContent = `${page.saved.total_count.toLocaleString()} completed downloads from this account on ${formatHistorySource(page.source)}.`;
            if (!append) els.historyAccountSavedList.replaceChildren();
            els.historyAccountSavedList.append(...page.saved.entries.map(savedRow));
            state.offset = (append ? state.offset : 0) + page.saved.entries.length;
            els.historyAccountMoreSavedBtn.hidden = !page.saved.has_more;
            if (page.insights?.profile_url) setProfile(page.insights.profile_url);
            els.historyAccountStatus.textContent = '';
            if (!append) void social.load();
        } catch (error) {
            appendLog(`[account] ${error}`, true);
            if (state.id === id && state.request === request)
                els.historyAccountStatus.textContent =
                    'This account could not be loaded. Return to the video and try again.';
        } finally {
            if (state.id === id && state.request === request) {
                state.reading = false;
                els.historyAccountMoreSavedBtn.disabled = false;
                els.historyAccountSavedList.setAttribute('aria-busy', 'false');
            }
        }
    };
    const renderVideoButtons = () => {
        const busy = state.remoteBusy || pendingVideos.has(state.id);
        for (const button of [els.historyAccountVideosBtn, els.historyAccountMoreVideosBtn]) {
            button.disabled = busy || !state.id || !api.available;
            button.setAttribute('aria-busy', String(busy));
        }
        els.historyAccountMoreVideosBtn.hidden = !state.remoteMore;
    };
    const loadVideos = async (append = false) => {
        const id = state.id;
        if (!id || pendingVideos.has(id) || !api.available) return;
        const request = state.request;
        pendingVideos.add(id);
        state.remoteBusy = true;
        renderVideoButtons();
        els.historyAccountVideosStatus.textContent = 'Loading profile videos…';
        try {
            const page = await api.loadHistoryAccountVideos({ id, offset: append ? state.remoteOffset : 0 });
            if (id !== state.id || request !== state.request) return;
            setProfile(page.profile_url);
            if (!append) els.historyAccountRemoteList.replaceChildren();
            const existing = new Set([...els.historyAccountRemoteList.children].map(row => row.dataset.url));
            for (const entry of page.entries) {
                if (existing.has(entry.url)) continue;
                existing.add(entry.url);
                const button = document.createElement('button');
                button.type = 'button';
                button.className = 'oj-panel oj-panel-compact oj-panel-interactive pinefetch-account-video';
                button.textContent = entry.title || entry.url;
                button.dataset.url = entry.url;
                button.setAttribute('aria-label', `Prepare download: ${entry.title || entry.url}`);
                button.addEventListener('click', () => prepareDownload(entry.url));
                els.historyAccountRemoteList.append(button);
            }
            state.remoteOffset = page.next_offset;
            state.remoteMore = page.has_more;
            els.historyAccountVideosStatus.textContent = els.historyAccountRemoteList.childElementCount
                ? `${els.historyAccountRemoteList.childElementCount} profile videos loaded.${page.has_more ? ' More videos are available.' : ''}`
                : 'No public videos were returned by this profile.';
        } catch (error) {
            appendLog(`[account videos] ${error}`, true);
            if (id === state.id && request === state.request)
                els.historyAccountVideosStatus.textContent =
                    'Profile videos could not be loaded. The platform may require sign-in or may not expose its video list. Try again later.';
        } finally {
            pendingVideos.delete(id);
            if (state.id === id) {
                state.remoteBusy = false;
                renderVideoButtons();
            }
        }
    };
    const open = id => {
        state.id = id;
        state.request += 1;
        state.reading = false;
        state.remoteBusy = false;
        state.offset = 0;
        state.remoteOffset = 0;
        state.remoteMore = false;
        setProfile('');
        els.historyAccountTitle.textContent = 'Account';
        els.historyAccountMeta.textContent = '';
        els.historyAccountSavedCount.textContent = '';
        els.historyAccountSavedList.replaceChildren();
        els.historyAccountRemoteList.replaceChildren();
        els.historyAccountMoreSavedBtn.hidden = true;
        els.historyAccountVideosStatus.textContent = '';
        social.reset(id);
        renderVideoButtons();
        void loadSaved();
        els.historyAccountBackBtn.focus({ preventScroll: true });
    };
    const bindEvents = () => {
        social.bindEvents();
        els.historyAccountBackBtn.addEventListener('click', back);
        els.historyAccountMoreSavedBtn.addEventListener('click', () => void loadSaved(true));
        els.historyAccountVideosBtn.addEventListener('click', () => void loadVideos());
        els.historyAccountMoreVideosBtn.addEventListener('click', () => void loadVideos(true));
        els.historyAccountOpenBtn.addEventListener('click', async () => {
            if (!state.profile) return;
            try {
                await api.openExternalUrl({ url: state.profile });
            } catch (error) {
                appendLog(`[account open] ${error}`, true);
            }
        });
    };
    const leave = () => {
        state.id = null;
        state.request += 1;
        state.reading = false;
        social.reset(null);
    };
    return Object.freeze({ open, bindEvents, leave });
};

export const createHistoryKeywordsView = ({ els, api, appendLog }) => {
    const state = { id: null, request: 0, content: null, loading: false, limit: 200, error: '' };
    const render = () => {
        const content = state.content;
        const query = els.historyKeywordsSearch.value.trim().toLocaleLowerCase();
        const source = els.historyKeywordsSource.value;
        const terms = (content?.terms || []).filter(
            term =>
                term.term.toLocaleLowerCase().includes(query) &&
                (source === 'all' || (source === 'shared' ? term.sources === 3 : term[source] > 0))
        );
        els.historyKeywordsRefreshBtn.disabled = !api.available || !state.id || state.loading;
        els.historyKeywordsRefreshBtn.setAttribute('aria-busy', String(state.loading));
        els.historyKeywordsSearch.disabled = state.loading || !content;
        els.historyKeywordsSource.disabled = state.loading || !content;
        els.historyKeywordsResults.setAttribute('aria-busy', String(state.loading));
        const sources = content?.sources;
        els.historyKeywordsMeta.textContent = sources
            ? [
                  sources.transcript ? 'Transcript available' : 'No saved transcript',
                  `${sources.ocr_frames.toLocaleString()} OCR ${sources.ocr_frames === 1 ? 'frame' : 'frames'}${sources.ocr_legacy ? ' + legacy text' : ''}`,
                  `${sources.captions.toLocaleString()} saved ${sources.captions === 1 ? 'caption' : 'captions'}`,
              ].join(' · ')
            : '';
        els.historyKeywordsStatus.textContent =
            state.error ||
            (state.loading
                ? 'Mining keywords…'
                : content
                  ? `${terms.length.toLocaleString()} ${terms.length === 1 ? 'keyword' : 'keywords'}${terms.length > state.limit ? ` · ${state.limit.toLocaleString()} shown` : ''}`
                  : '');
        els.historyKeywordsEmpty.hidden = terms.length > 0 || state.loading || Boolean(state.error);
        els.historyKeywordsEmpty.textContent = content?.terms.length
            ? 'No keywords match these filters.'
            : sources && !sources.transcript && !sources.ocr_frames && !sources.ocr_legacy && !sources.captions
              ? 'No saved text is available. Create a transcript, run OCR or save captions first.'
              : 'No keywords occur at least twice in the saved texts.';
        els.historyKeywordsTable.hidden = !terms.length;
        els.historyKeywordsLoadMoreBtn.hidden = terms.length <= state.limit;
        els.historyKeywordsLoadMoreBtn.disabled = state.loading;
        const fragment = document.createDocumentFragment();
        for (const term of terms.slice(0, state.limit)) {
            const row = document.createElement('tr');
            const label = document.createElement('th');
            label.scope = 'row';
            label.textContent = term.term;
            row.append(label);
            for (const source of ['transcript', 'ocr', 'captions', 'total']) {
                const count = document.createElement('td');
                count.className = 'oj-numeric';
                count.textContent = term[source] ? term[source].toLocaleString() : '—';
                if (!term[source]) count.setAttribute('aria-label', 'Not found');
                row.append(count);
            }
            fragment.append(row);
        }
        els.historyKeywordsRows.replaceChildren(fragment);
    };
    const reset = id => {
        Object.assign(state, { id, content: null, loading: false, limit: 200, error: '' });
        state.request++;
        els.historyKeywordsSearch.value = '';
        els.historyKeywordsSource.value = 'all';
        render();
    };
    const load = async () => {
        if (!state.id || state.loading || !api.available) return;
        const id = state.id;
        const request = ++state.request;
        state.loading = true;
        state.error = '';
        render();
        try {
            const content = await api.getHistoryKeywords({ id });
            if (id !== state.id || request !== state.request) return;
            state.content = content;
            state.limit = 200;
        } catch (error) {
            appendLog(`[Keywords] ${error}`, true);
            if (id === state.id && request === state.request)
                state.error = /limit/i.test(String(error))
                    ? 'The saved text is too large for keyword mining.'
                    : `Keywords could not be loaded. Try refreshing.${state.content ? ' Previous results are still shown.' : ''}`;
        } finally {
            if (id === state.id && request === state.request) {
                state.loading = false;
                render();
            }
        }
    };
    const bindEvents = () => {
        els.historyKeywordsRefreshBtn.addEventListener('click', () => void load());
        for (const element of [els.historyKeywordsSearch, els.historyKeywordsSource]) {
            element.addEventListener(element === els.historyKeywordsSearch ? 'input' : 'change', () => {
                state.limit = 200;
                render();
            });
        }
        els.historyKeywordsLoadMoreBtn.addEventListener('click', () => {
            state.limit += 200;
            render();
        });
    };
    return Object.freeze({ reset, load, bindEvents });
};

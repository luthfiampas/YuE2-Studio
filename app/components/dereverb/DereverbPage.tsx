import React, { useCallback, useEffect, useRef, useState } from 'react';
import { AudioLines, Check, Download, FileAudio, Loader2, Music, Sparkles, Trash2, Upload, X } from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { SELF_READ } from '../../services/studioQueries';
import { apiUrl } from '../../services/apiBase';
import { saveFile } from '../../services/saveFile';

/**
 * De-reverb.
 *
 * One model, one run: a vocal stem goes in, the dry take comes out beside the
 * reverb that was removed. Its own runs and history; it shares the Python
 * environment with voice swap and nothing else.
 */

const CARD = 'rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/3';
const LABEL = 'text-[11px] font-bold uppercase tracking-wide text-zinc-500';

interface Stem {
  song_id: string;
  title: string;
  stem: string;
}

interface Run {
  id: string;
  source: string;
  created: string;
  status: 'running' | 'done' | 'error';
  error?: string | null;
}

interface Job {
  kind: string;
  subject: string;
  stage: string;
  progress: number;
  log: string[];
  error?: string | null;
  done: boolean;
}

interface Status {
  installed: boolean;
  runtime_installed: boolean;
  busy: boolean;
  job: Job | null;
}

type Reach = 'ok' | 'missing' | 'unreachable';

const clock = (seconds: number) => `${Math.floor(seconds / 60)}:${String(Math.round(seconds % 60)).padStart(2, '0')}`;
const date = (text: string) => {
  const parsed = new Date(text);
  return Number.isNaN(parsed.getTime()) ? text : parsed.toLocaleDateString();
};

/// The log of whatever job this page is running, wherever it is shown.
function JobLog({ job, open, onToggle }: { job: Job; open: boolean; onToggle: () => void }) {
  if (job.log.length === 0) return null;
  return (
    <div className="mt-2">
      <button type="button" onClick={onToggle} className="text-[11px] font-semibold text-zinc-500 hover:text-pink-600 dark:text-zinc-400">
        {open ? 'Hide the log' : `Show the log (${job.log.length} lines)`}
      </button>
      {open && (
        <pre className="mt-1 max-h-48 overflow-auto rounded-lg bg-zinc-950/95 p-2 text-[11px] leading-4 text-zinc-200">
          {job.log.slice(-60).join('\n')}
        </pre>
      )}
    </div>
  );
}

export function DereverbPage(): React.ReactElement {
  const [status, setStatus] = useState<Status | null>(null);
  const [reach, setReach] = useState<Reach>('ok');
  const [stems, setStems] = useState<Stem[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [uploaded, setUploaded] = useState<File | null>(null);
  const [stemId, setStemId] = useState('');
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [showLog, setShowLog] = useState(false);
  const [confirming, setConfirming] = useState<{ title: string; message: string; run: () => void } | null>(null);

  const picker = useRef<HTMLInputElement | null>(null);

  const read = useCallback(
    (path: string) =>
      fetch(path, { headers: SELF_READ })
        .then(response => (response.ok ? response.json() : null))
        .catch(() => null),
    [],
  );

  const readStatus = useCallback(async () => {
    const response = await fetch('/v1/dereverb/status', { headers: SELF_READ }).catch(() => null);
    const body = response && response.ok ? await response.json().catch(() => null) : null;
    setReach(response === null ? 'unreachable' : response.status === 404 ? 'missing' : body ? 'ok' : 'unreachable');
    setStatus(body ? (body as Status) : null);
  }, []);

  const readLists = useCallback(async () => {
    const [available, history] = await Promise.all([read('/v1/dereverb/stems'), read('/v1/dereverb/jobs')]);
    setStems(Array.isArray(available) ? (available as Stem[]) : []);
    setRuns(Array.isArray(history) ? (history as Run[]) : []);
  }, [read]);

  const job = status?.job && !status.job.done ? status.job : null;
  const percent = (value: number) => Math.round(value * 100);

  useEffect(() => {
    void readStatus();
    void readLists();
    const tick = window.setInterval(() => void readStatus(), job ? 1500 : 4000);
    return () => window.clearInterval(tick);
  }, [readStatus, readLists, job !== null]);

  // The run record is the source of truth for a run. The job reports done a
  // moment before the record is written, so ask until the record agrees.
  const running = runs.some(run => run.status === 'running');
  useEffect(() => {
    if (!running) return;
    const tick = window.setInterval(() => void readLists(), 2000);
    return () => window.clearInterval(tick);
  }, [running, readLists]);

  const openRun = async () => {
    if (!uploaded && !stemId) return;
    setBusy(true);
    setProblem(null);
    try {
      const form = new FormData();
      if (uploaded) form.append('file', uploaded, uploaded.name);
      else form.append('song', stemId);
      if (!uploaded) form.append('stem', 'vocals');
      const response = await fetch('/v1/dereverb/jobs', { method: 'POST', body: form });
      if (!response.ok) {
        const payload = await response.json().catch(() => ({}));
        throw new Error(payload?.error ?? `HTTP ${response.status}`);
      }
      setUploaded(null);
      setStemId('');
      await readStatus();
      await readLists();
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    setProblem(null);
    const response = await fetch('/v1/dereverb/install', { method: 'POST' }).catch(() => null);
    if (!response || !response.ok) {
      const payload = await response?.json().catch(() => ({}));
      setProblem(payload?.error ?? 'the install could not be started');
    }
    await readStatus();
  };

  const chosen = stems.find(stem => stem.song_id === stemId) ?? null;
  const source = uploaded ? uploaded.name : chosen ? `${chosen.title} - ${chosen.stem}` : '';
  const ready = status?.installed === true;
  const canRun = ready && Boolean(uploaded || stemId) && !job && !busy;

  return (
    <div className="flex-1 overflow-y-auto bg-white px-5 py-6 dark:bg-suno md:px-8">
      <div className="mx-auto max-w-4xl space-y-5">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.18em] text-pink-500">De-reverb</p>
          <h1 className="mt-1 text-2xl font-bold text-zinc-950 dark:text-white">Take the room out of a vocal</h1>
          <p className="mt-2 max-w-3xl text-sm text-zinc-600 dark:text-zinc-400">
            One model, one pass: a vocal stem in, the dry take and the reverb that was removed out. Reverb is the part a
            separator cannot help with, and it is what a voice model learns if you train on a wet stem.
          </p>
        </div>

        <section className={CARD}>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
                <Sparkles size={16} className="text-pink-500" /> De-reverb model
              </div>
              <p className="mt-1 text-xs leading-5 text-zinc-500 dark:text-zinc-400">
                {ready
                  ? 'UVR-DeEcho-DeReverb installed · 214 MB'
                  : job?.kind === 'install'
                    ? `${job.stage} · ${percent(job.progress)}%`
                    : reach === 'missing'
                      ? 'This server build has no de-reverb. Rebuild the service and upload it again.'
                      : reach === 'unreachable'
                        ? 'The studio service is not answering.'
                        : job?.kind === 'runtime'
                          ? 'The Python runtime is being installed under Settings, Python runtime.'
                          : status && !status.runtime_installed
                            ? 'The Python runtime is not installed — it is set up under Settings, Python runtime.'
                            : 'Not installed · UVR-DeEcho-DeReverb · 214 MB'}
              </p>
              {status?.job?.error && status.job.done && (
                <p className="mt-1 text-xs text-red-600 dark:text-red-400">{status.job.error}</p>
              )}
              {(job || status?.busy) && (
                <button
                  type="button"
                  onClick={() => void fetch('/v1/dereverb/cancel', { method: 'POST' }).then(readStatus).catch(() => undefined)}
                  className="mt-1 text-[11px] font-semibold text-zinc-500 hover:text-red-500"
                >
                  Cancel this job
                </button>
              )}
            </div>
            {ready ? (
              <span className="inline-flex items-center gap-2 text-xs font-semibold text-emerald-600 dark:text-emerald-400">
                <Check size={14} /> Installed
              </span>
            ) : job || reach !== 'ok' ? null : status && !status.runtime_installed ? (
              <button
                type="button"
                onClick={() => window.dispatchEvent(new CustomEvent('yue:open-settings', { detail: 'python' }))}
                className="rounded-lg border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-200"
              >
                Open Settings, Python runtime
              </button>
            ) : (
              <button
                type="button"
                onClick={() => void install()}
                className="inline-flex items-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-3 py-1.5 text-xs font-bold text-white"
              >
                Install
              </button>
            )}
          </div>
          {job?.kind === 'install' && (
            <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
              <div className="h-full rounded-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent(job.progress)}%` }} />
            </div>
          )}
          {job?.kind === 'install' && <JobLog job={job} open={showLog} onToggle={() => setShowLog(open => !open)} />}
          {problem && <p className="mt-2 text-xs text-red-600 dark:text-red-400">{problem}</p>}
        </section>

        <section className={CARD}>
          <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
            <FileAudio size={16} className="text-pink-500" /> 1 · The vocal stem
          </div>
          {uploaded ? (
            <div className="mt-3 flex items-center gap-2 rounded-lg border border-pink-300 bg-pink-500/5 px-3 py-2 dark:border-pink-400/30">
              <FileAudio size={14} className="shrink-0 text-pink-500" />
              <span className="min-w-0 flex-1 truncate text-sm text-zinc-800 dark:text-zinc-200">{uploaded.name}</span>
              <button
                type="button"
                disabled={Boolean(job)}
                onClick={() => setUploaded(null)}
                className="shrink-0 text-zinc-400 hover:text-red-500 disabled:opacity-30"
              >
                <X size={13} />
              </button>
            </div>
          ) : (
            <button
              type="button"
              disabled={busy || Boolean(job)}
              onClick={() => picker.current?.click()}
              className="mt-3 flex w-full flex-col items-center gap-1 rounded-xl border border-dashed border-zinc-300 px-4 py-5 text-center hover:border-pink-400 disabled:opacity-50 dark:border-white/15"
            >
              <Upload size={18} className="text-zinc-400" />
              <span className="text-sm font-semibold text-zinc-700 dark:text-zinc-200">Upload a vocal stem</span>
            </button>
          )}
          <input
            ref={picker}
            type="file"
            accept="audio/*"
            className="hidden"
            onChange={event => {
              const file = event.target.files?.[0];
              if (file) {
                setStemId('');
                setUploaded(file);
              }
              event.target.value = '';
            }}
          />

          <div className="mt-3">
            <span className={LABEL}>Or one from the library</span>
            {stems.length === 0 ? (
              <p className="mt-1 text-[11px] text-zinc-500">No vocal stems in the library yet — separate a song in Studio tools first.</p>
            ) : (
              <div className="mt-1 max-h-40 overflow-y-auto rounded-lg border border-zinc-200 dark:border-white/10">
                {stems.map(stem => {
                  const picked = !uploaded && stem.song_id === stemId;
                  return (
                    <button
                      key={`${stem.song_id}-${stem.stem}`}
                      type="button"
                      disabled={Boolean(job)}
                      onClick={() => {
                        setUploaded(null);
                        setStemId(stem.song_id);
                      }}
                      className={`flex w-full items-center gap-2 border-b border-zinc-100 px-3 py-2 text-left last:border-b-0 dark:border-white/5 ${
                        picked ? 'bg-pink-500/10' : 'hover:bg-zinc-100 dark:hover:bg-white/5'
                      }`}
                    >
                      <Music size={13} className={picked ? 'shrink-0 text-pink-500' : 'shrink-0 text-zinc-400'} />
                      <span className="min-w-0 flex-1 truncate text-sm text-zinc-800 dark:text-zinc-200">{stem.title}</span>
                      <span className="shrink-0 text-[11px] text-zinc-500">{stem.stem}</span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </section>

        <section className={CARD}>
          <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
            <AudioLines size={16} className="text-pink-500" /> 2 · Run
          </div>
          <p className="mt-2 text-sm leading-6 text-zinc-600 dark:text-zinc-300">
            UVR-DeEcho-DeReverb · about 3 minutes per 30 s on the processor, seconds on the card.
          </p>
          {job?.kind === 'run' ? (
            <div className="mt-3 rounded-xl border border-pink-200 bg-pink-500/5 px-3 py-3 dark:border-pink-400/20">
              <div className="flex items-center justify-between text-xs">
                <span className="inline-flex items-center gap-2 font-semibold text-zinc-800 dark:text-zinc-100">
                  <Loader2 size={13} className="animate-spin text-pink-500" /> {job.stage} · {percent(job.progress)}%
                </span>
                <button
                  type="button"
                  onClick={() => void fetch('/v1/dereverb/cancel', { method: 'POST' }).then(readStatus).catch(() => undefined)}
                  className="font-semibold text-zinc-500 hover:text-pink-600"
                >
                  Cancel
                </button>
              </div>
              <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
                <div className="h-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent(job.progress)}%` }} />
              </div>
              <JobLog job={job} open={showLog} onToggle={() => setShowLog(open => !open)} />
            </div>
          ) : (
            <button
              type="button"
              onClick={() => void openRun()}
              disabled={!canRun}
              className="mt-3 inline-flex items-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
            >
              <Sparkles size={14} /> Remove reverb
            </button>
          )}
          {!job && !canRun && (
            <p className="mt-2 text-[11px] text-zinc-500">
              {!ready ? 'Install the model above first.' : !uploaded && !stemId ? 'Choose a vocal stem.' : 'A job is running.'}
            </p>
          )}
        </section>

        <section className={CARD}>
          <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
            <AudioLines size={16} className="text-pink-500" /> 3 · History
          </div>
          <p className="mt-2 text-sm leading-6 text-zinc-600 dark:text-zinc-300">
            Every run, with the dry take and the reverb it removed. Nothing is written into a song.
          </p>
          {runs.length === 0 ? (
            <p className="mt-3 text-xs text-zinc-500">Nothing yet. A finished run lands here.</p>
          ) : (
            <div className="mt-3 space-y-2">
              {runs.map(run => (
                <div key={run.id} className="rounded-xl border border-zinc-200 px-3 py-2 dark:border-white/10">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="min-w-0 flex-1 truncate text-sm font-semibold text-zinc-800 dark:text-zinc-100">{run.source}</span>
                    <span className="shrink-0 text-[11px] text-zinc-500">
                      {date(run.created)} · {run.status}
                    </span>
                    {run.status === 'done' && (
                      <>
                        <button
                          type="button"
                          onClick={() =>
                            void saveFile(`${run.source.replace(/\.(wav|flac|mp3|m4a|ogg|opus)$/i, '')} - dry.wav`, { url: apiUrl(`/v1/dereverb/jobs/${run.id}/dry`) }).catch(error =>
                              setProblem(error instanceof Error ? error.message : String(error)),
                            )
                          }
                          className="inline-flex items-center gap-1 rounded-lg border border-zinc-200 px-2 py-1 text-[11px] font-semibold text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300"
                        >
                          <Download size={11} /> Dry
                        </button>
                        <button
                          type="button"
                          onClick={() =>
                            void saveFile(`${run.source.replace(/\.(wav|flac|mp3|m4a|ogg|opus)$/i, '')} - reverb.wav`, { url: apiUrl(`/v1/dereverb/jobs/${run.id}/reverb`) }).catch(error =>
                              setProblem(error instanceof Error ? error.message : String(error)),
                            )
                          }
                          className="inline-flex items-center gap-1 rounded-lg border border-zinc-200 px-2 py-1 text-[11px] font-semibold text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300"
                        >
                          <Download size={11} /> Reverb
                        </button>
                      </>
                    )}
                    <button
                      type="button"
                      disabled={job?.kind === 'run' && job.subject === run.id}
                      onClick={() =>
                        setConfirming({
                          title: 'Delete this run?',
                          message: `“${run.source}” and both files are removed from the history.`,
                          run: () =>
                            void fetch(`/v1/dereverb/jobs/${run.id}`, { method: 'DELETE' })
                              .then(async response => {
                                if (!response.ok) {
                                  const body = await response.json().catch(() => ({}));
                                  setProblem(body?.error ?? `HTTP ${response.status}`);
                                  return;
                                }
                                await readLists();
                              })
                              .catch(error => setProblem(error instanceof Error ? error.message : String(error))),
                        })
                      }
                      className="text-zinc-400 hover:text-red-500 disabled:opacity-30"
                      title="Delete this run"
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                  {run.status === 'error' && <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">{run.error}</p>}
                  {run.status === 'running' && <p className="mt-1 text-[11px] text-zinc-500">Still running…</p>}
                  {run.status === 'done' && (
                    <div className="mt-2 grid gap-2 sm:grid-cols-2">
                      <div>
                        <p className="mb-1 text-[11px] font-semibold text-zinc-500">Dry</p>
                        <audio src={apiUrl(`/v1/dereverb/jobs/${run.id}/dry`)} controls preload="none" className="h-8 w-full" />
                      </div>
                      <div>
                        <p className="mb-1 text-[11px] font-semibold text-zinc-500">Removed reverb</p>
                        <audio src={apiUrl(`/v1/dereverb/jobs/${run.id}/reverb`)} controls preload="none" className="h-8 w-full" />
                      </div>
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </section>
      </div>

      <ConfirmDialog
        isOpen={confirming !== null}
        title={confirming?.title ?? ''}
        message={confirming?.message ?? ''}
        confirmLabel="Delete"
        onConfirm={() => {
          confirming?.run();
          setConfirming(null);
        }}
        onCancel={() => setConfirming(null)}
      />
    </div>
  );
}

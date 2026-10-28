import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  AlertTriangle,
  AudioLines,
  Check,
  Download,
  FileAudio,
  Loader2,
  MicVocal,
  Music,
  Play,
  Plus,
  Trash2,
  Upload,
  X,
} from 'lucide-react';
import { ConfirmDialog } from '../ConfirmDialog';
import { SELF_READ } from '../../services/studioQueries';
import { apiUrl } from '../../services/apiBase';
import { saveFile } from '../../services/saveFile';

/**
 * Voice swap.
 *
 * A tool of its own: teach it a voice from recordings, then hand it a vocal
 * stem and get the same performance back in that voice. Nothing here reads or
 * writes the song library - a swap is a file in this tool's own history.
 *
 * The engine is RVC, installed on request and run as one process per job; this
 * page only ever talks to `/v1/voice-swap/*`.
 */

const CARD = 'rounded-2xl border border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/3';
const CONTROL =
  'w-full rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 outline-hidden focus:border-pink-500 disabled:opacity-50 dark:border-white/10 dark:bg-black/30 dark:text-white';
const LABEL = 'text-[11px] font-bold uppercase tracking-wide text-zinc-500';

interface Recording {
  name: string;
  seconds: number;
}

interface Voice {
  id: string;
  name: string;
  created: string;
  epochs: number;
  save_every: number;
  used: number;
  checkpoints: number[];
  recordings: Recording[];
  minutes: number;
  trained: boolean;
}

interface StemChoice {
  song_id: string;
  title: string;
  stem: string;
}

interface Swap {
  id: string;
  source: string;
  voice: string;
  voice_name: string;
  pitch: number;
  index_rate: number;
  protect: number;
  rms_mix: number;
  created: string;
  status: 'running' | 'done' | 'error';
  error?: string | null;
}

interface Job {
  kind: string;
  subject: string;
  stage: string;
  progress: number;
  detail?: string | null;
  log: string[];
  error?: string | null;
  done: boolean;
}

interface EngineStatus {
  installed: boolean;
  /** Python and torch: shared by every Python tool, installed from Settings. */
  runtime_installed: boolean;
  missing: string[];
  busy: boolean;
  root: string;
  elapsed: number;
  job: Job | null;
}

/** The server's own sentence when it sends one; the status line otherwise. */
const reason = async (response: Response) => {
  const text = await response.text().catch(() => '');
  try {
    const parsed = JSON.parse(text) as { error?: string };
    if (parsed?.error) return parsed.error;
  } catch {
    // not JSON: the rejection's own text is the reason
  }
  return text.trim().slice(0, 200) || `HTTP ${response.status}`;
};

/// The swap settings are remembered between visits: pitch 0 unless the user
/// moved it, so a first swap never transposes by accident.
const SWAP_KEY = 'yue2:voice-swap:swap';
const storedSwap = (): { pitch: number; indexRate: number; protect: number; rmsMix: number } => {
  try {
    const value = JSON.parse(localStorage.getItem(SWAP_KEY) ?? '{}');
    return {
      pitch: Number.isFinite(value.pitch) ? value.pitch : 0,
      indexRate: Number.isFinite(value.indexRate) ? value.indexRate : 0.75,
      protect: Number.isFinite(value.protect) ? value.protect : 0.33,
      rmsMix: Number.isFinite(value.rmsMix) ? value.rmsMix : 0.25,
    };
  } catch {
    return { pitch: 0, indexRate: 0.75, protect: 0.33, rmsMix: 0.25 };
  }
};

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
      <button
        type="button"
        onClick={onToggle}
        className="text-[11px] font-semibold text-zinc-500 hover:text-pink-600 dark:text-zinc-400"
      >
        {open ? 'Hide the log' : `Show the log (${job.log.length} lines)`}
      </button>
      {open && (
        <pre className="mt-1 max-h-56 overflow-auto rounded-lg bg-zinc-950/95 p-2 text-[11px] leading-4 text-zinc-200">
          {job.log.slice(-60).join('\n')}
        </pre>
      )}
    </div>
  );
}

export function VoiceSwapPage(): React.ReactElement {
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  // Why the engine card cannot ask the server: no service, or a build without
  // these routes. They look the same to fetch and mean different things.
  const [engineProblem, setEngineProblem] = useState<'missing' | 'unreachable' | null>(null);
  const job = engine?.job && !engine.job.done ? engine.job : null;
  const [voices, setVoices] = useState<Voice[]>([]);
  // The recordings that are not a voice yet. A voice only exists once training
  // has produced a model, which is what the page shows as the draft.
  const [draft, setDraft] = useState<Voice | null>(null);
  const [swaps, setSwaps] = useState<Swap[]>([]);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [tab, setTab] = useState<'voices' | 'swap' | 'history'>('voices');

  const [epochs, setEpochs] = useState(20);
  const [saveEvery, setSaveEvery] = useState(5);
  const [name, setName] = useState('');
  const [voiceId, setVoiceId] = useState('');
  const [initial] = useState(storedSwap);
  const [pitch, setPitch] = useState(initial.pitch);
  const [indexRate, setIndexRate] = useState(initial.indexRate);
  const [protect, setProtect] = useState(initial.protect);
  const [rmsMix, setRmsMix] = useState(initial.rmsMix);
  const [stem, setStem] = useState<File | null>(null);
  // A vocal stem already separated in the library, instead of an upload.
  const [stems, setStems] = useState<StemChoice[]>([]);
  const [stemChoice, setStemChoice] = useState<StemChoice | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [showLog, setShowLog] = useState(false);
  const [confirming, setConfirming] = useState<{ title: string; message: string; confirmLabel: string; run: () => void } | null>(null);
  /** A dialog for keeping some checkpoints and deleting the rest. */
  const [pruning, setPruning] = useState<{ id: string; name: string; used: number; checkpoints: number[]; keep: number[] } | null>(null);

  const recordingsPicker = useRef<HTMLInputElement | null>(null);
  // A ref, not `busy`: two drops in the same tick both read the old state and
  // each would make its own voice.
  const uploading = useRef(false);
  const stemPicker = useRef<HTMLInputElement | null>(null);

  // One read, tolerant of a service that is not answering.
  const read = useCallback(
    (path: string) =>
      fetch(path, { headers: SELF_READ })
        .then(response => (response.ok ? response.json() : null))
        .catch(() => null),
    [],
  );

  const readStatus = useCallback(async () => {
    const response = await fetch('/v1/voice-swap/status', { headers: SELF_READ }).catch(() => null);
    const status = response && response.ok ? await response.json().catch(() => null) : null;
    setEngineProblem(
      response === null ? 'unreachable' : response.status === 404 ? 'missing' : status ? null : 'unreachable',
    );
    setEngine(status ? (status as EngineStatus) : null);
  }, []);

  /// Voices, the draft and the history. Not the status (polled on its own) and
  /// not the stems (only the Swap tab needs those).
  const refresh = useCallback(async () => {
    const [list, waiting, history] = await Promise.all([
      read('/v1/voice-swap/voices'),
      read('/v1/voice-swap/draft'),
      read('/v1/voice-swap/swaps'),
    ]);
    const known: Voice[] = Array.isArray(list) ? (list as Voice[]) : [];
    setVoices(known);
    setDraft(waiting && typeof waiting === 'object' && 'id' in waiting ? (waiting as Voice) : null);
    setSwaps(Array.isArray(history) ? (history as Swap[]) : []);
    setVoiceId(current => (known.some(voice => voice.id === current) ? current : known[0]?.id ?? ''));
  }, [read]);

  const readStems = useCallback(async () => {
    const available = await read('/v1/voice-swap/stems');
    setStems(Array.isArray(available) ? (available as StemChoice[]) : []);
  }, [read]);

  // The status is small and is what moves while a job runs; everything else
  // changes only when a job ends or the user acts, so it is not re-fetched
  // three times a second.
  const wasRunning = useRef(false);
  useEffect(() => {
    if (wasRunning.current && !job) {
      void refresh();
      void readStems();
    }
    wasRunning.current = Boolean(job);
  }, [job, refresh, readStems]);

  useEffect(() => {
    void readStatus();
    void refresh();
    const tick = window.setInterval(() => void readStatus(), job ? 1500 : 4000);
    return () => window.clearInterval(tick);
  }, [readStatus, refresh, job !== null]);

  // A swap's own record is written just after the job reports done, so the
  // history is asked again until the record itself says it finished.
  const running = swaps.some(item => item.status === 'running');
  useEffect(() => {
    if (!running) return;
    const tick = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(tick);
  }, [running, refresh]);

  useEffect(() => {
    if (tab === 'swap') void readStems();
  }, [tab, readStems]);

  // Only the voice a job is actually using is locked; the others stay usable.
  const lockedVoice =
    job?.kind === 'train'
      ? job.subject
      : job?.kind === 'convert'
        ? swaps.find(swap => swap.id === job.subject)?.voice ?? null
        : null;

  useEffect(() => {
    try {
      localStorage.setItem(SWAP_KEY, JSON.stringify({ pitch, indexRate, protect, rmsMix }));
    } catch {
      // a browser that refuses storage simply forgets the settings
    }
  }, [pitch, indexRate, protect, rmsMix]);

  const chosen = voices.find(voice => voice.id === voiceId) ?? null;
  const ready = engine?.installed === true;
  const percent = (value: number) => Math.round(value * 100);
  const minutes = draft?.minutes ?? 0;
  const enough = minutes >= 5;
  const checkpoints = Math.max(1, Math.ceil(epochs / Math.max(1, saveEvery)));

  // The draft owns its name and its training settings. Read them once per
  // draft: a poll must not overwrite what is being typed, and a reload or a
  // page change must not put the defaults back.
  const hydrated = useRef('');
  useEffect(() => {
    if (!draft || hydrated.current === draft.id) return;
    hydrated.current = draft.id;
    setName(draft.name);
    setEpochs(draft.epochs || 20);
    setSaveEvery(draft.save_every || 5);
  }, [draft]);

  /// Name and training settings are the draft's, so they survive a reload.
  const rememberTraining = async () => {
    if (!draft) return;
    try {
      await mutate(`/v1/voice-swap/voices/${draft.id}`, 'PATCH', {
        name,
        epochs,
        save_every: saveEvery,
      });
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    }
  };

  const mutate = async (path: string, method: string, body?: unknown) => {
    const response = await fetch(path, {
      method,
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) throw new Error(await reason(response));
    return response.json().catch(() => ({}));
  };

  const post = async (path: string, body?: unknown) => {
    const response = await fetch(path, {
      method: 'POST',
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) throw new Error(await reason(response));
    return response.json().catch(() => ({}));
  };

  const uploadRecordings = async (files: FileList | null) => {
    if (!files || files.length === 0 || uploading.current) return;
    uploading.current = true;
    setBusy(true);
    setProblem(null);
    try {
      const target = draft ?? (await post('/v1/voice-swap/voices', { name: name.trim() || 'My voice' }));
      const form = new FormData();
      Array.from(files).forEach(file => form.append('file', file, file.name));
      const response = await fetch(`/v1/voice-swap/voices/${target.id}/recordings`, { method: 'POST', body: form });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(payload?.error ?? `HTTP ${response.status}`);
      await refresh();
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    } finally {
      uploading.current = false;
      setBusy(false);
    }
  };

  const renameTrained = async (voice: Voice, next: string) => {
    if (!next.trim() || next.trim() === voice.name) {
      setRenaming(null);
      return;
    }
    try {
      await mutate(`/v1/voice-swap/voices/${voice.id}`, 'PATCH', { name: next.trim() });
      setRenaming(null);
      await refresh();
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    }
  };

  const train = async () => {
    if (!draft) return;
    setProblem(null);
    try {
      await post(`/v1/voice-swap/voices/${draft.id}/train`, {
        name: name.trim() || 'My voice',
        epochs,
        save_every: saveEvery,
      });
      setTab('voices');
      await refresh();
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    }
  };

  const swap = async () => {
    if (!(stem ?? stemChoice) || !chosen) return;
    setBusy(true);
    setProblem(null);
    try {
      const form = new FormData();
      form.append('voice', chosen.id);
      form.append('pitch', String(pitch));
      form.append('index_rate', String(indexRate));
      form.append('protect', String(protect));
      form.append('rms_mix', String(rmsMix));
      if (stem) {
        form.append('file', stem, stem.name);
      } else if (stemChoice) {
        form.append('song', stemChoice.song_id);
        form.append('stem', stemChoice.stem);
      }
      const response = await fetch('/v1/voice-swap/swaps', { method: 'POST', body: form });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(payload?.error ?? `HTTP ${response.status}`);
      setStem(null);
      setStemChoice(null);
      setTab('history');
      await refresh();
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  // Only this tool's own engine jobs belong on this card; the Python runtime
  // install is Settings' job and is shown there.
  const engineJob = job && (job.kind === 'install' || job.kind === 'selftest') ? job : null;
  const canTrain = ready && Boolean(draft) && enough && !job && !busy;
  const canSwap = ready && Boolean(stem ?? stemChoice) && Boolean(chosen) && !job && !busy;

  return (
    <div className="flex-1 overflow-y-auto bg-white px-5 py-6 dark:bg-suno md:px-8">
      <div className="mx-auto max-w-4xl space-y-5">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.18em] text-pink-500">Voice swap</p>
            <h1 className="mt-1 text-2xl font-bold text-zinc-950 dark:text-white">Sing a stem in another voice</h1>
            <p className="mt-2 max-w-3xl text-sm text-zinc-600 dark:text-zinc-400">
              Train a voice from recordings of it, then give it a vocal stem and get the same performance back in that
              voice. Timing, words and expression stay; the timbre changes.
            </p>
          </div>
        </div>

        {/* The engine: one optional download, run on the server. */}
        <section className={CARD}>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
                <MicVocal size={17} className="text-pink-500" /> Voice swap engine
              </div>
              <p className="mt-1 text-xs leading-5 text-zinc-500 dark:text-zinc-400">
                {ready
                  ? `Ready · ${engine?.root ?? ''}`
                  : engineJob
                    ? `${engineJob.stage} · ${percent(engineJob.progress)}% · ${clock(engine?.elapsed ?? 0)}`
                    : engineProblem === 'missing'
                      ? 'This server build has no voice swap. Rebuild the service and upload it again.'
                      : engine === null
                        ? 'The studio service is not answering.'
                        : !engine.runtime_installed
                          ? 'The Python runtime is not installed — it is set up once under Settings, Python runtime.'
                          : `RVC is not installed · ${engine.missing.join(', ')}`}
              </p>
              {engineJob?.error && engineJob.done && (
                <p className="mt-1 text-xs leading-5 text-red-600 dark:text-red-400">{engineJob.error}</p>
              )}
              {engineJob && (
                <button
                  type="button"
                  onClick={() => void post('/v1/voice-swap/cancel').then(refresh).catch(() => undefined)}
                  className="mt-1 text-[11px] font-semibold text-zinc-500 hover:text-red-500"
                >
                  Cancel this job
                </button>
              )}
            </div>
            {ready ? (
              <div className="flex items-center gap-3">
                {!job && !busy && (
                  <button
                    type="button"
                    onClick={() => void post('/v1/voice-swap/selftest').then(refresh).catch(() => undefined)}
                    className="inline-flex items-center gap-2 rounded-lg border border-zinc-200 px-2.5 py-1 text-xs font-semibold text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300"
                  >
                    Test it
                  </button>
                )}
                <span className="inline-flex items-center gap-2 text-xs font-semibold text-emerald-600 dark:text-emerald-400">
                  <Check size={14} /> Installed
                </span>
              </div>
            ) : engine && !engine.runtime_installed ? (
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
                disabled={engine === null}
                onClick={() => void post('/v1/voice-swap/install').then(refresh).catch(() => undefined)}
                className="inline-flex items-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-3 py-1.5 text-xs font-bold text-white disabled:opacity-50"
              >
                <Play size={12} /> Install
              </button>
            )}
          </div>
          {engineJob && (
            <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
              <div className="h-full rounded-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent(engineJob.progress)}%` }} />
            </div>
          )}
          {engineJob && <JobLog job={engineJob} open={showLog} onToggle={() => setShowLog(open => !open)} />}
        </section>

        {problem && (
          <p className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-300">
            {problem}
          </p>
        )}

        <div className="flex gap-1 rounded-xl border border-zinc-200 bg-zinc-50 p-1 dark:border-white/10 dark:bg-white/3">
          {(
            [
              ['voices', '1 · Voices'],
              ['swap', '2 · Swap'],
              ['history', `3 · History${swaps.length ? ` (${swaps.length})` : ''}`],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              onClick={() => setTab(id)}
              className={`flex-1 rounded-lg px-3 py-2 text-sm font-semibold transition-colors ${
                tab === id ? 'bg-white text-zinc-900 shadow-sm dark:bg-white/10 dark:text-white' : 'text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200'
              }`}
            >
              {label}
            </button>
          ))}
        </div>

        {tab === 'voices' && (
          <div className="space-y-5">
            {/* Recordings are a draft: no voice exists until training has made
                a model from them. */}
            <section className={CARD}>
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
                  <Upload size={17} className="text-pink-500" /> Your recordings
                </div>
                {draft && (
                  <button
                    type="button"
                    disabled={lockedVoice === draft?.id}
                    onClick={() =>
                      setConfirming({
                        title: 'Discard these recordings?',
                        message: 'The draft and the files in it are removed. Nothing trained is affected.',
                        confirmLabel: 'Discard',
                        run: () => void mutate(`/v1/voice-swap/voices/${draft.id}`, 'DELETE').then(refresh).catch(error => setProblem(String(error.message))),
                      })
                    }
                    className="text-xs font-semibold text-zinc-500 hover:text-red-500"
                  >
                    Discard
                  </button>
                )}
              </div>
              <p className="mt-2 text-sm leading-6 text-zinc-600 dark:text-zinc-300">
                The same voice, the same room, dry. Sing - not only speech - across the range you want back. Ten minutes is
                usable, twenty is good.
              </p>

              <label className="mt-3 block">
                <span className={LABEL}>This voice will be called</span>
                <input
                  value={name}
                  disabled={lockedVoice === draft?.id}
                  onChange={event => setName(event.target.value)}
                  onBlur={() => void rememberTraining()}
                  placeholder="My voice"
                  aria-label="Voice name"
                  className={`${CONTROL} mt-1`}
                />
              </label>

              <button
                type="button"
                disabled={busy || lockedVoice === draft?.id}
                onClick={() => recordingsPicker.current?.click()}
                onDragOver={event => event.preventDefault()}
                onDrop={event => {
                  event.preventDefault();
                  void uploadRecordings(event.dataTransfer.files);
                }}
                className="mt-3 flex w-full flex-col items-center gap-1 rounded-xl border border-dashed border-zinc-300 px-4 py-6 text-center hover:border-pink-400 disabled:opacity-50 dark:border-white/15"
              >
                {busy ? <Loader2 size={20} className="animate-spin text-pink-500" /> : <FileAudio size={20} className="text-zinc-400" />}
                <span className="text-sm font-semibold text-zinc-700 dark:text-zinc-200">
                  {busy ? 'Uploading…' : job ? 'A job is running…' : 'Drop recordings or choose files'}
                </span>
                <span className="text-[11px] text-zinc-500">WAV or FLAC, mono, 44.1 or 48 kHz</span>
              </button>
              <input
                ref={recordingsPicker}
                type="file"
                accept="audio/*"
                multiple
                disabled={busy || lockedVoice === draft?.id}
                className="hidden"
                onChange={event => {
                  void uploadRecordings(event.target.files);
                  event.target.value = '';
                }}
              />

              {!draft || draft.recordings.length === 0 ? (
                <p className="mt-2 text-[11px] text-zinc-500">Nothing uploaded yet. No voice is made until you train.</p>
              ) : (
                <div className="mt-3 space-y-3">
                  <div className="flex items-center justify-between text-xs">
                    <span className="font-semibold text-zinc-700 dark:text-zinc-200">
                      {draft.recordings.length} recordings · {draft.minutes.toFixed(1)} min
                    </span>
                    <span className={enough ? 'text-emerald-600 dark:text-emerald-400' : 'text-amber-600 dark:text-amber-300'}>
                      {enough ? 'Enough to train' : 'Needs 5 min or more'}
                    </span>
                  </div>
                  <div className="overflow-hidden rounded-lg border border-zinc-200 dark:border-white/10">
                    {draft.recordings.map(recording => (
                      <div key={recording.name} className="flex items-center gap-2 border-b border-zinc-100 px-3 py-2 last:border-b-0 dark:border-white/5">
                        {recording.seconds <= 0 ? (
                          <AlertTriangle size={13} className="shrink-0 text-zinc-400" />
                        ) : recording.seconds < 15 ? (
                          <AlertTriangle size={13} className="shrink-0 text-amber-500" />
                        ) : (
                          <Check size={13} className="shrink-0 text-emerald-500" />
                        )}
                        <span className="min-w-0 flex-1 truncate text-sm text-zinc-800 dark:text-zinc-200">{recording.name}</span>
                        <span className="shrink-0 text-[11px] text-zinc-500">
                          {recording.seconds > 0 ? clock(recording.seconds) : 'length unreadable'}
                        </span>
                        <button
                          type="button"
                          disabled={lockedVoice === draft?.id}
                          onClick={() =>
                            setConfirming({
                              title: `Remove ${recording.name}?`,
                              message: 'It is dropped from the draft. Nothing trained is affected.',
                              confirmLabel: 'Remove',
                              run: () =>
                                void mutate(`/v1/voice-swap/voices/${draft.id}/recordings/${encodeURIComponent(recording.name)}`, 'DELETE')
                                  .then(refresh)
                                  .catch(error => setProblem(String(error.message))),
                            })
                          }
                          className="shrink-0 text-zinc-400 hover:text-red-500 disabled:cursor-not-allowed disabled:opacity-30 disabled:hover:text-zinc-400"
                          title={job ? 'A job is running' : 'Remove this recording'}
                        >
                          <X size={13} />
                        </button>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              <div className="mt-4 grid gap-3 sm:grid-cols-2">
                <label className="block">
                  <span className={LABEL}>Epochs</span>
                  <input
                    type="number"
                    min={1}
                    max={1200}
                    value={epochs}
                    disabled={lockedVoice === draft?.id}
                    onChange={event => setEpochs(Math.max(1, Math.min(1200, Number(event.target.value) || 1)))}
                    onBlur={() => void rememberTraining()}
                    className={`${CONTROL} mt-1`}
                  />
                  <span className="text-[11px] text-zinc-500">{epochs} passes over the recordings</span>
                </label>
                <label className="block">
                  <span className={LABEL}>Save every</span>
                  <input
                    type="number"
                    min={1}
                    max={50}
                    value={saveEvery}
                    disabled={lockedVoice === draft?.id}
                    onChange={event => setSaveEvery(Math.max(1, Math.min(50, Number(event.target.value) || 1)))}
                    onBlur={() => void rememberTraining()}
                    className={`${CONTROL} mt-1`}
                  />
                  <span className="text-[11px] text-zinc-500">{checkpoints} checkpoints, each a usable model</span>
                </label>
              </div>
              <p className="mt-3 text-[11px] leading-4 text-zinc-500">
                Fixed: 48 kHz, pitch guidance on, pretrained weights, search index built afterwards. More epochs do not
                always sound better - too many and it copies your room and breath instead of your voice.
              </p>

              {job?.kind === 'train' ? (
                <div className="mt-3 rounded-xl border border-pink-200 bg-pink-500/5 px-3 py-3 dark:border-pink-400/20">
                  <div className="flex items-center justify-between text-xs">
                    <span className="inline-flex items-center gap-2 font-semibold text-zinc-800 dark:text-zinc-100">
                      <Loader2 size={13} className="animate-spin text-pink-500" /> {job.stage} · {percent(job.progress)}%
                    </span>
                    <button type="button" onClick={() => void post('/v1/voice-swap/cancel').then(refresh)} className="font-semibold text-zinc-500 hover:text-pink-600">
                      Cancel
                    </button>
                  </div>
                  <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
                    <div className="h-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent(job.progress)}%` }} />
                  </div>
                  <JobLog job={job} open={showLog} onToggle={() => setShowLog(open => !open)} />
                </div>
              ) : (
                <>
                  <button
                    type="button"
                    onClick={() => void train()}
                    disabled={!canTrain}
                    className="mt-3 inline-flex items-center justify-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    <MicVocal size={14} /> Train this voice
                  </button>
                  {!canTrain && (
                    <p className="mt-2 text-[11px] text-zinc-500">
                      {!ready
                        ? 'Install the engine above first.'
                        : !draft
                          ? 'Upload recordings first.'
                          : !enough
                            ? 'Add at least 5 minutes of recordings.'
                            : 'A job is running.'}
                    </p>
                  )}
                </>
              )}
            </section>

            <section className={CARD}>
              <div className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
                <AudioLines size={17} className="text-pink-500" /> Trained voices
              </div>
              {voices.length === 0 ? (
                <p className="mt-2 text-xs text-zinc-500">None yet. A voice appears here once training has made a model.</p>
              ) : (
                <div className="mt-3 space-y-2">
                  {voices.map(voice => (
                    <div key={voice.id} className="rounded-xl border border-zinc-200 px-3 py-2 dark:border-white/10">
                      <div className="flex flex-wrap items-center gap-2">
                        <MicVocal size={14} className="shrink-0 text-pink-500" />
                        {renaming === voice.id ? (
                          <input
                            autoFocus
                            defaultValue={voice.name}
                            onBlur={event => void renameTrained(voice, event.target.value)}
                            onKeyDown={event => {
                              if (event.key === 'Enter') event.currentTarget.blur();
                              if (event.key === 'Escape') setRenaming(null);
                            }}
                            className="min-w-0 flex-1 rounded-md border border-pink-400 bg-white px-1.5 py-0.5 text-sm font-semibold text-zinc-900 outline-hidden dark:bg-black/30 dark:text-white"
                          />
                        ) : (
                          <span className="min-w-0 flex-1 truncate text-sm font-semibold text-zinc-800 dark:text-zinc-100">
                            {voice.name}
                          </span>
                        )}
                        <button
                          type="button"
                          disabled={lockedVoice === voice.id}
                          onClick={() => setRenaming(voice.id)}
                          className="text-[11px] font-semibold text-zinc-500 hover:text-pink-600 disabled:opacity-30"
                        >
                          Rename
                        </button>
                        <button
                          type="button"
                          onClick={() => {
                            setVoiceId(voice.id);
                            setTab('swap');
                          }}
                          className="rounded-lg border border-zinc-200 px-2.5 py-1 text-xs font-semibold text-zinc-700 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-200"
                        >
                          Use in swap
                        </button>
                        <button
                          type="button"
                          disabled={lockedVoice === voice.id}
                          onClick={() =>
                            setConfirming({
                              title: `Delete “${voice.name}”?`,
                              message: 'The trained model and its checkpoints are removed. This cannot be undone.',
                              confirmLabel: 'Delete voice',
                              run: () => void mutate(`/v1/voice-swap/voices/${voice.id}`, 'DELETE').then(refresh).catch(error => setProblem(String(error.message))),
                            })
                          }
                          className="text-zinc-400 hover:text-red-500"
                          title="Delete this voice"
                        >
                          <Trash2 size={13} />
                        </button>
                      </div>
                      <p className="mt-1 text-[11px] text-zinc-500">
                        {voice.epochs > 0 ? `${voice.epochs} epochs · ` : ''}
                        {voice.save_every > 0 ? `every ${voice.save_every} · ` : ''}
                        {voice.checkpoints.length} checkpoints · {voice.minutes.toFixed(1)} min · created {date(voice.created)}
                      </p>
                      <div className="mt-2 flex flex-wrap items-center gap-1.5">
                        <button
                          type="button"
                          disabled={lockedVoice === voice.id}
                          onClick={() =>
                            setPruning({
                              id: voice.id,
                              name: voice.name,
                              used: voice.used,
                              checkpoints: voice.checkpoints,
                              keep: voice.checkpoints,
                            })
                          }
                          className="mr-1 rounded-lg border border-zinc-200 px-2 py-0.5 text-[11px] font-semibold text-zinc-600 hover:border-pink-400 hover:text-pink-600 disabled:opacity-30 dark:border-white/10 dark:text-zinc-300"
                          title="Keep some checkpoints, delete the rest"
                        >
                          Clean up
                        </button>
                        <span className="text-[11px] text-zinc-500">Swaps use</span>
                        {voice.checkpoints.map(epoch => (
                          <span key={epoch} className="inline-flex items-center">
                            <button
                              type="button"
                              disabled={lockedVoice === voice.id}
                              onClick={() => {
                                void mutate(`/v1/voice-swap/voices/${voice.id}`, 'PATCH', { used: epoch })
                                  .then(refresh)
                                  .catch(error => setProblem(String(error.message)));
                              }}
                              className={`rounded-full border px-2 py-0.5 text-[11px] font-semibold ${
                                voice.used === epoch
                                  ? 'border-pink-400 bg-pink-500/10 text-zinc-900 dark:text-white'
                                  : 'border-zinc-200 text-zinc-500 dark:border-white/10'
                              }`}
                            >
                              epoch {epoch}
                            </button>
                            {voice.checkpoints.length > 1 && (
                              <button
                                type="button"
                                disabled={lockedVoice === voice.id}
                                aria-label={`Delete the model saved at epoch ${epoch}`}
                                onClick={() =>
                                  setConfirming({
                                    title: `Delete the model at epoch ${epoch}?`,
                                    message: `${voice.name} keeps its recordings and its other checkpoints. ${
                                      voice.used === epoch
                                        ? `Swaps will use epoch ${voice.checkpoints.filter(other => other !== epoch).slice(-1)[0]} instead.`
                                        : 'The checkpoint swaps use today is unchanged.'
                                    }`,
                                    confirmLabel: 'Delete model',
                                    run: () =>
                                      void mutate(`/v1/voice-swap/voices/${voice.id}/checkpoints/${epoch}`, 'DELETE')
                                        .then(refresh)
                                        .catch(error => setProblem(String(error.message))),
                                  })
                                }
                                className="ml-0.5 text-zinc-300 hover:text-red-500 dark:text-zinc-600"
                                title="Delete this saved model"
                              >
                                <X size={10} />
                              </button>
                            )}
                          </span>
                        ))}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </section>
          </div>
        )}

        {tab === 'swap' && (
          <section className={CARD}>
            <p className="text-sm leading-6 text-zinc-600 dark:text-zinc-300">
              The isolated vocal, without the band - then the voice it should come back in.
            </p>

            {stem ? (
              <div className="mt-3 flex items-center gap-2 rounded-lg border border-pink-300 bg-pink-500/5 px-3 py-2 dark:border-pink-400/30">
                <FileAudio size={14} className="shrink-0 text-pink-500" />
                <span className="min-w-0 flex-1 truncate text-sm text-zinc-800 dark:text-zinc-200">{stem.name}</span>
                <button type="button" onClick={() => setStem(null)} className="shrink-0 text-zinc-400 hover:text-red-500">
                  <X size={13} />
                </button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => stemPicker.current?.click()}
                className="mt-3 flex w-full flex-col items-center gap-1 rounded-xl border border-dashed border-zinc-300 px-4 py-5 text-center hover:border-pink-400 dark:border-white/15"
              >
                <Upload size={18} className="text-zinc-400" />
                <span className="text-sm font-semibold text-zinc-700 dark:text-zinc-200">Upload a vocal stem</span>
              </button>
            )}
            <input
              ref={stemPicker}
              type="file"
              accept="audio/*"
              className="hidden"
              onChange={event => {
                setStem(event.target.files?.[0] ?? null);
                if (event.target.files?.[0]) setStemChoice(null);
                event.target.value = '';
              }}
            />

            {stems.length > 0 && (
              <div className="mt-3">
                <span className={LABEL}>Or a vocal stem you already have</span>
                <div className="mt-1 max-h-40 overflow-y-auto rounded-lg border border-zinc-200 dark:border-white/10">
                  {stems.map(item => {
                    const picked = !stem && stemChoice?.song_id === item.song_id;
                    return (
                      <button
                        key={`${item.song_id}-${item.stem}`}
                        type="button"
                        onClick={() => {
                          setStem(null);
                          setStemChoice(item);
                        }}
                        className={`flex w-full items-center gap-2 border-b border-zinc-100 px-3 py-2 text-left last:border-b-0 dark:border-white/5 ${
                          picked ? 'bg-pink-500/10' : 'hover:bg-zinc-100 dark:hover:bg-white/5'
                        }`}
                      >
                        <Music size={13} className={picked ? 'shrink-0 text-pink-500' : 'shrink-0 text-zinc-400'} />
                        <span className="min-w-0 flex-1 truncate text-sm text-zinc-800 dark:text-zinc-200">{item.title}</span>
                        <span className="shrink-0 text-[11px] text-zinc-500">{item.stem}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="mt-4">
              <span className={LABEL}>Voice</span>
              <div className="mt-1 flex flex-wrap gap-2">
                {voices.filter(voice => voice.trained).map(voice => (
                  <button
                    key={voice.id}
                    type="button"
                    onClick={() => setVoiceId(voice.id)}
                    className={`inline-flex items-center gap-2 rounded-full border px-3 py-1.5 text-xs font-semibold ${
                      voice.id === voiceId ? 'border-pink-400 bg-pink-500/10 text-zinc-900 dark:text-white' : 'border-zinc-200 text-zinc-500 dark:border-white/10'
                    }`}
                  >
                    {voice.id === voiceId && <Check size={10} strokeWidth={3} />}
                    {voice.name}
                  </button>
                ))}
                {voices.every(voice => !voice.trained) && <span className="text-xs text-zinc-500">Train a voice first.</span>}
              </div>
              {chosen && (
                <p className="mt-2 text-[11px] text-zinc-500">
                  Using the model saved at epoch {chosen.used} of {chosen.epochs}. Change it under Voices if this one sounds off.
                </p>
              )}
            </div>

            <div className="mt-3 rounded-xl border border-zinc-200 px-3 py-3 dark:border-white/10">
              <p className="text-sm font-semibold text-zinc-800 dark:text-zinc-100">
                Pitch shift: {pitch > 0 ? '+' : ''}{pitch} semitones
              </p>
              <p className="mt-1 text-[11px] leading-4 text-zinc-500">
                The voice keeps the stem's melody, so a voice of another range needs the shift: -12 is an octave down,
                +12 an octave up. Start at 0 if the two voices sit in the same range.
              </p>
              <input type="range" min={-24} max={24} value={pitch} onChange={event => setPitch(Number(event.target.value))} className="mt-3 w-full accent-pink-500" />
            </div>

            <div className="mt-3 grid gap-3 sm:grid-cols-3">
              <label className="block">
                <span className={LABEL}>Index rate · {indexRate.toFixed(2)}</span>
                <input type="range" min={0} max={1} step={0.05} value={indexRate} onChange={event => setIndexRate(Number(event.target.value))} className="mt-2 w-full accent-pink-500" />
                <span className="text-[11px] text-zinc-500">More of the trained timbre, more artefacts</span>
              </label>
              <label className="block">
                <span className={LABEL}>Protect · {protect.toFixed(2)}</span>
                <input type="range" min={0} max={0.5} step={0.01} value={protect} onChange={event => setProtect(Number(event.target.value))} className="mt-2 w-full accent-pink-500" />
                <span className="text-[11px] text-zinc-500">Keeps breath and consonants · 0.5 is off</span>
              </label>
              <label className="block">
                <span className={LABEL}>Dynamics · {rmsMix.toFixed(2)}</span>
                <input type="range" min={0} max={1} step={0.05} value={rmsMix} onChange={event => setRmsMix(Number(event.target.value))} className="mt-2 w-full accent-pink-500" />
                <span className="text-[11px] text-zinc-500">Lower keeps the performance's own volume</span>
              </label>
            </div>

            <button
              type="button"
              onClick={() => void swap()}
              disabled={!canSwap}
              className="mt-4 inline-flex items-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-4 py-2 text-sm font-bold text-white disabled:cursor-not-allowed disabled:opacity-50"
            >
              {job?.kind === 'convert' ? <Loader2 size={14} className="animate-spin" /> : <AudioLines size={14} />}
              {job?.kind === 'convert' ? `Swapping · ${percent(job.progress)}%` : 'Swap the whole stem'}
            </button>
            {!canSwap && (
              <p className="mt-2 text-[11px] text-zinc-500">
                {!ready ? 'Install the engine above first.' : !chosen ? 'Train a voice first.' : !(stem ?? stemChoice) ? 'Choose a vocal stem.' : 'A job is running.'}
              </p>
            )}
            {job?.kind === 'convert' && (
              <div className="mt-3 space-y-1">
                <div className="h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
                  <div className="h-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent(job.progress)}%` }} />
                </div>
                <button
                  type="button"
                  onClick={() => void post('/v1/voice-swap/cancel').then(refresh).catch(() => undefined)}
                  className="text-[11px] font-semibold text-zinc-500 hover:text-red-500"
                >
                  Cancel this job
                </button>
                <JobLog job={job} open={showLog} onToggle={() => setShowLog(open => !open)} />
              </div>
            )}
          </section>
        )}

        {tab === 'history' && (
          <section className={CARD}>
            <p className="text-sm leading-6 text-zinc-600 dark:text-zinc-300">
              Every swap this tool has made, with the settings it ran with. Each is a file of its own; no song is created or
              changed by any of it.
            </p>
            {swaps.length === 0 ? (
              <p className="mt-3 text-xs text-zinc-500">Nothing swapped yet. A finished swap lands here.</p>
            ) : (
              <div className="mt-3 space-y-2">
                {swaps.map(item => (
                  <div key={item.id} className="rounded-xl border border-zinc-200 px-3 py-2 dark:border-white/10">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="min-w-0 flex-1 truncate text-sm font-semibold text-zinc-800 dark:text-zinc-100">
                        {item.source} → {item.voice_name}
                      </span>
                      <span className="shrink-0 text-[11px] text-zinc-500">
                        {date(item.created)} · {item.status}
                      </span>
                      {item.status === 'done' && (
                        <button
                          type="button"
                          onClick={() =>
                            void saveFile(`${item.source.replace(/\.(wav|flac|mp3|m4a|ogg|opus)$/i, '')} - ${item.voice_name}.wav`, {
                              url: apiUrl(`/v1/voice-swap/swaps/${item.id}/audio`),
                            }).catch(error => setProblem(error instanceof Error ? error.message : String(error)))
                          }
                          className="inline-flex items-center gap-1 rounded-lg border border-zinc-200 px-2 py-1 text-[11px] font-semibold text-zinc-600 hover:border-pink-400 hover:text-pink-600 dark:border-white/10 dark:text-zinc-300"
                        >
                          <Download size={11} /> WAV
                        </button>
                      )}
                      <button
                        type="button"
                        disabled={job?.kind === 'convert' && job.subject === item.id}
                        onClick={() =>
                          setConfirming({
                            title: 'Delete this swap?',
                            message: `“${item.source} → ${item.voice_name}” and its file are removed from the history.`,
                            confirmLabel: 'Delete swap',
                            run: () => void mutate(`/v1/voice-swap/swaps/${item.id}`, 'DELETE').then(refresh).catch(error => setProblem(String(error.message))),
                          })
                        }
                        className="text-zinc-400 hover:text-red-500 disabled:cursor-not-allowed disabled:opacity-30 disabled:hover:text-zinc-400"
                        title={job?.kind === 'convert' && job.subject === item.id ? 'This swap is still running' : 'Delete this swap'}
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                    <p className="mt-1 text-[11px] text-zinc-500">
                      pitch {item.pitch > 0 ? '+' : ''}{item.pitch} · index {item.index_rate.toFixed(2)} · protect{' '}
                      {item.protect.toFixed(2)} · dynamics {item.rms_mix.toFixed(2)}
                    </p>
                    {item.status === 'done' && (
                      <div className="mt-2">
                        {/* The native player is the scrubber; decoding every
                            result for a waveform meant several full-file
                            fetches each time the history opened. */}
                        <audio src={apiUrl(`/v1/voice-swap/swaps/${item.id}/audio`)} controls preload="none" className="mt-2 h-8 w-full" />
                      </div>
                    )}
                    {item.status === 'error' && <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">{item.error}</p>}
                    {item.status === 'running' && <p className="mt-1 text-[11px] text-zinc-500">Still running…</p>}
                  </div>
                ))}
              </div>
            )}
          </section>
        )}
      </div>

      {pruning && (
        <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs" onClick={event => { if (event.target === event.currentTarget) setPruning(null); }}>
          <div className="w-full max-w-md rounded-2xl border border-zinc-200 bg-white p-5 shadow-2xl dark:border-white/10 dark:bg-zinc-900">
            <h3 className="text-base font-semibold text-zinc-900 dark:text-white">Clean up “{pruning.name}”</h3>
            <p className="mt-1 text-sm leading-5 text-zinc-500 dark:text-zinc-400">
              Keep the models you want; the rest are deleted. Each is about 55 MB.
            </p>
            <div className="mt-3 flex items-center justify-between text-[11px] font-semibold">
              <label className="inline-flex cursor-pointer items-center gap-1.5 text-zinc-600 dark:text-zinc-300">
                <input
                  type="checkbox"
                  checked={pruning.keep.length === pruning.checkpoints.length}
                  onChange={event => setPruning(current => (current ? { ...current, keep: event.target.checked ? current.checkpoints : [current.used] } : current))}
                  className="h-3.5 w-3.5 accent-pink-500"
                />
                Select all
              </label>
              <span className="text-zinc-500">at least one stays</span>
            </div>
            <div className="mt-3 max-h-56 overflow-y-auto rounded-lg border border-zinc-200 dark:border-white/10">
              {pruning.checkpoints.map(epoch => {
                const kept = pruning.keep.includes(epoch);
                return (
                  <label
                    key={epoch}
                    className="flex cursor-pointer items-center gap-2 border-b border-zinc-100 px-3 py-2 text-sm last:border-b-0 dark:border-white/5"
                  >
                    <input
                      type="checkbox"
                      checked={kept}
                      disabled={kept && pruning.keep.length === 1}
                      onChange={() =>
                        setPruning(current =>
                          current
                            ? {
                                ...current,
                                keep: kept
                                  ? current.keep.filter(value => value !== epoch)
                                  : [...current.keep, epoch].sort((a, b) => a - b),
                              }
                            : current,
                        )
                      }
                      className="h-4 w-4 accent-pink-500"
                    />
                    <span className="min-w-0 flex-1 text-zinc-800 dark:text-zinc-200">epoch {epoch}</span>
                    {epoch === pruning.used && (
                      <span className="rounded-full bg-pink-500/10 px-2 py-0.5 text-[11px] font-semibold text-pink-600 dark:text-pink-300">in use</span>
                    )}
                  </label>
                );
              })}
            </div>
            <div className="mt-4 flex items-center justify-end gap-2">
              <button type="button" onClick={() => setPruning(null)} className="rounded-lg border border-zinc-200 px-3 py-1.5 text-sm font-semibold text-zinc-600 hover:bg-zinc-50 dark:border-white/10 dark:text-zinc-300 dark:hover:bg-white/5">
                Cancel
              </button>
              <button
                type="button"
                disabled={pruning.keep.length === 0}
                onClick={() => {
                  const target = pruning;
                  setPruning(null);
                  setConfirming({
                    title: `Delete ${target.checkpoints.length - target.keep.length} checkpoint${target.checkpoints.length - target.keep.length === 1 ? '' : 's'}?`,
                    message: `Only the kept models stay with “${target.name}”. A deleted checkpoint cannot be trained again.`,
                    confirmLabel: 'Delete',
                    run: () =>
                      void mutate(`/v1/voice-swap/voices/${target.id}/checkpoints/prune`, 'POST', { keep: target.keep })
                        .then(refresh)
                        .catch(error => setProblem(error instanceof Error ? error.message : String(error))),
                  });
                }}
                className="rounded-lg bg-red-600 px-3 py-1.5 text-sm font-semibold text-white disabled:opacity-40"
              >
                Delete {pruning.checkpoints.length - pruning.keep.length}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog
        isOpen={confirming !== null}
        title={confirming?.title ?? ''}
        message={confirming?.message ?? ''}
        confirmLabel={confirming?.confirmLabel}
        onConfirm={() => {
          confirming?.run();
          setConfirming(null);
        }}
        onCancel={() => setConfirming(null)}
      />
    </div>
  );
}

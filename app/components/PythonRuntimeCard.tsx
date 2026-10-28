import React, { useCallback, useEffect, useState } from 'react';
import { Check } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { SELF_READ } from '../services/studioQueries';

/**
 * The shared Python runtime.
 *
 * Python and torch are the half every Python tool needs - voice swap, and the
 * de-reverb later. It is the slow install (gigabytes, tens of minutes on the
 * network volume), and it belongs to the models page like every other optional
 * download, not inside the tool that happened to need it first.
 */

interface Job {
  kind: string;
  stage: string;
  progress: number;
  log: string[];
  error?: string | null;
  done: boolean;
}

interface Status {
  installed: boolean;
  runtime_installed: boolean;
  job: Job | null;
}

export const PythonRuntimeCard: React.FC = () => {
  const { t } = useI18n();
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showLog, setShowLog] = useState(false);

  const load = useCallback(async () => {
    const response = await fetch('/v1/voice-swap/status', { headers: SELF_READ }).catch(() => null);
    if (response && response.ok) {
      setStatus((await response.json().catch(() => null)) as Status | null);
    }
  }, []);

  useEffect(() => {
    void load();
    const tick = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(tick);
  }, [load]);

  const install = async () => {
    setError(null);
    const response = await fetch('/v1/voice-swap/install?runtime=true', { method: 'POST' }).catch(() => null);
    if (!response || !response.ok) {
      const body = await response?.json().catch(() => null);
      setError(body?.error ?? 'the install could not be started');
    }
    await load();
  };

  const cancel = async () => {
    await fetch('/v1/voice-swap/cancel', { method: 'POST' }).catch(() => undefined);
    await load();
  };

  // This section owns installs, not a song being trained or a swap: the job
  // slot is shared, but its progress is not this page's to show.
  // Only the runtime install: an engine install is a different card, and a
  // training run is not this page's business at all.
  const job = status?.job && !status.job.done && status.job.kind === 'runtime' ? status.job : null;
  const installed = status?.runtime_installed === true;
  const percent = Math.round((job?.progress ?? 0) * 100);

  return (
    <div className="max-w-xl space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm leading-5 text-zinc-600 dark:text-zinc-300">
            {installed
              ? t('pythonRuntimeReady' as never)
              : job
                ? `${job.stage} · ${percent}%`
                : t('pythonRuntimeNeedsIt' as never)}
          </p>
          {status?.job?.kind === 'runtime' && status.job.error && status.job.done && (
            <p className="mt-1 text-xs text-red-600 dark:text-red-400">{status.job.error}</p>
          )}
          {error && <p className="mt-1 text-xs text-red-600 dark:text-red-400">{error}</p>}
        </div>
        {installed ? (
          <span className="inline-flex items-center gap-2 text-xs font-semibold text-emerald-600 dark:text-emerald-400">
            <Check size={14} /> {t('installed')}
          </span>
        ) : job ? (
          <button type="button" onClick={() => void cancel()} className="text-xs font-semibold text-zinc-500 hover:text-pink-600">
            {t('cancel')}
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void install()}
            className="inline-flex items-center gap-2 rounded-lg bg-linear-to-r from-orange-500 to-pink-600 px-3 py-1.5 text-xs font-bold text-white"
          >
            {t('install')}
          </button>
        )}
      </div>
      {job && (
        <div className="h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10">
          <div className="h-full rounded-full bg-linear-to-r from-orange-500 to-pink-600 transition-all" style={{ width: `${percent}%` }} />
        </div>
      )}
      {job?.log && job.log.length > 0 && (
        <div>
          <button type="button" onClick={() => setShowLog(open => !open)} className="text-[11px] font-semibold text-zinc-500 hover:text-pink-600 dark:text-zinc-400">
            {showLog ? t('hideLog' as never) : `${t('showLog' as never)} (${job.log.length})`}
          </button>
          {showLog && (
            <pre className="mt-1 max-h-48 overflow-auto rounded-lg bg-zinc-950/95 p-2 text-[11px] leading-4 text-zinc-200">
              {job.log.slice(-60).join('\n')}
            </pre>
          )}
        </div>
      )}
    </div>
  );
};


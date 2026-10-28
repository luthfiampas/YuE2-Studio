import React, { useEffect, useState } from 'react';
import { MicVocal, Sparkles } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { VoiceSwapPage } from './voiceSwap/VoiceSwapPage';
import { DereverbPage } from './dereverb/DereverbPage';

/**
 * The tools that are not part of making a song.
 *
 * One entry in the window's sidebar, a list of tools inside it, and the tool
 * itself on the right: the studio's own navigation stays short, and a tool can
 * arrive without another top-level page. De-reverb lands here next.
 */

type ToolId = 'voice-swap' | 'de-reverb';

interface Tool {
  id: ToolId;
  label: string;
  icon: React.ReactNode;
  page: React.ReactNode;
}

const TOOL_IDS: ToolId[] = ['voice-swap', 'de-reverb'];

/** The tool in the URL, or the first one when it names nothing known. */
const fromPath = (): ToolId => {
  const named = /^\/misc\/([a-z-]+)/.exec(window.location.pathname)?.[1] as ToolId | undefined;
  return named && TOOL_IDS.includes(named) ? named : 'voice-swap';
};

export function MiscPage(): React.ReactElement {
  const { t } = useI18n();
  const [tool, setTool] = useState<ToolId>(fromPath);

  // A tool is a place: /misc/voice-swap can be refreshed, bookmarked and sent.
  useEffect(() => {
    const listen = () => setTool(fromPath());
    window.addEventListener('popstate', listen);
    const named = /^\/misc\/([a-z-]+)/.exec(window.location.pathname)?.[1];
    if (window.location.pathname === '/misc' || (named && !TOOL_IDS.includes(named as ToolId))) {
      window.history.replaceState({}, '', `/misc/${fromPath()}`);
    }
    return () => window.removeEventListener('popstate', listen);
  }, []);

  const open = (next: ToolId) => {
    setTool(next);
    window.history.pushState({}, '', `/misc/${next}`);
  };

  const tools: Tool[] = [
    {
      id: 'voice-swap',
      label: t('voiceSwap'),
      icon: <MicVocal size={15} />,
      page: <VoiceSwapPage />,
    },
    {
      id: 'de-reverb',
      label: 'De-reverb',
      icon: <Sparkles size={15} />,
      page: <DereverbPage />,
    },
  ];
  const active = tools.find(entry => entry.id === tool) ?? tools[0];

  return (
    <div className="flex h-full min-h-0 w-full">
      <nav className="flex w-48 shrink-0 flex-col gap-1 overflow-y-auto border-r border-zinc-200 p-3 dark:border-white/10">
        <p className="px-2 pb-1 text-[11px] font-bold uppercase tracking-wide text-zinc-500">{t('misc')}</p>
        {tools.map(entry => (
          <button
            key={entry.id}
            type="button"
            onClick={() => open(entry.id)}
            className={`flex items-center gap-2 rounded-lg px-2 py-2 text-left text-sm font-medium transition-colors ${
              entry.id === tool
                ? 'bg-zinc-100 text-zinc-900 dark:bg-white/10 dark:text-white'
                : 'text-zinc-500 hover:bg-zinc-50 hover:text-zinc-800 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-100'
            }`}
          >
            {entry.icon}
            <span className="min-w-0 truncate">{entry.label}</span>
          </button>
        ))}
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">{active.page}</div>
    </div>
  );
}

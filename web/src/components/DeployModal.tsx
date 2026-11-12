import React, { useEffect, useRef, useState } from 'react';
import {
  Code,
  FolderGit2,
  Layers,
  Loader2,
  Plus,
  Rocket,
  Shield,
  Square,
  Trash,
  X,
} from 'lucide-react';
import {
  BuildError,
  buildDeployForm,
  buildRequestFor,
  cancelBuild,
  fetchBuildStatus,
  formatElapsed,
  requestBuild,
  type BuildStatus,
  validateServiceName,
  type EnvPair,
} from '../lib/deployPackage';

interface DeployModalProps {
  isOpen: boolean;
  onClose: () => void;
  onDeploySuccess: () => void;
}

export const DeployModal: React.FC<DeployModalProps> = ({
  isOpen,
  onClose,
  onDeploySuccess,
}) => {
  const [activeTab, setActiveTab] = useState<'git' | 'dockerfile' | 'image'>('git');
  const [name, setName] = useState('');
  const [gitUrl, setGitUrl] = useState('');
  const [dockerImage, setDockerImage] = useState('');
  const [dockerfileContent, setDockerfileContent] = useState(
    'FROM python:3.11-slim\nWORKDIR /app\nCOPY . .\nRUN pip install -r requirements.txt\nEXPOSE 8080\nCMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8080"]'
  );
  const [port, setPort] = useState(8080);
  const [cooldown, setCooldown] = useState(300);
  const [isPublic, setIsPublic] = useState(true);
  const [localUpstream, setLocalUpstream] = useState(false);
  const [envPairs, setEnvPairs] = useState<EnvPair[]>([
    { key: 'PORT', value: '8080', isSecret: false },
  ]);
  const [contextUrl, setContextUrl] = useState('');
  const [phase, setPhase] = useState<'building' | 'deploying' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [buildLog, setBuildLog] = useState<string[]>([]);
  const isSubmitting = phase !== null;
  // Live build progress, polled while the build request is pending.
  const [buildStatus, setBuildStatus] = useState<BuildStatus | null>(null);
  const [buildStartedAt, setBuildStartedAt] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  const [stopping, setStopping] = useState(false);
  const buildName = useRef('');
  const logRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (phase !== 'building') return;
    const token = sessionStorage.getItem('amoeba_token') || localStorage.getItem('amoeba_token');
    let active = true;
    const poll = async () => {
      const status = await fetchBuildStatus(buildName.current, token);
      if (active && status) setBuildStatus(status);
    };
    poll();
    const statusTimer = setInterval(poll, 2000);
    const clock = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      active = false;
      clearInterval(statusTimer);
      clearInterval(clock);
    };
  }, [phase]);

  // Keep the newest log line in view.
  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [buildStatus]);

  if (!isOpen) return null;

  const handleStopBuild = async () => {
    setStopping(true);
    const token = sessionStorage.getItem('amoeba_token') || localStorage.getItem('amoeba_token');
    try {
      await cancelBuild(buildName.current, token);
    } catch {
      // The build request reports the outcome either way.
    }
  };

  const handleAddEnv = () => {
    setEnvPairs([...envPairs, { key: '', value: '', isSecret: false }]);
  };

  const handleRemoveEnv = (index: number) => {
    setEnvPairs(envPairs.filter((_, i) => i !== index));
  };

  const handleEnvChange = (index: number, field: 'key' | 'value' | 'isSecret', value: any) => {
    const updated = [...envPairs];
    updated[index] = { ...updated[index], [field]: value };
    setEnvPairs(updated);
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const nameError = validateServiceName(name.trim());
    if (nameError) {
      setError(nameError);
      return;
    }

    setError(null);
    setBuildLog([]);

    try {
      const token = sessionStorage.getItem('amoeba_token') || localStorage.getItem('amoeba_token');

      // Git and Dockerfile sources are built into an image on the Amoeba host first.
      let image = dockerImage.trim();
      const build = buildRequestFor(activeTab, {
        gitUrl,
        dockerfile: dockerfileContent,
        contextUrl,
      });
      if (build) {
        buildName.current = name.trim();
        setBuildStatus(null);
        setBuildStartedAt(Date.now());
        setNow(Date.now());
        setStopping(false);
        setPhase('building');
        image = await requestBuild(name.trim(), build, token);
      }

      setPhase('deploying');
      const formData = buildDeployForm({
        name: name.trim(),
        image,
        description: `Deployed via Amoeba Web UI (${activeTab})`,
        port: Number(port),
        cooldownSeconds: Number(cooldown),
        isPublic,
        localUpstream,
        envPairs,
      });

      const res = await fetch('/admin/apps', {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${token}`
        },
        body: formData,
      });

      if (!res.ok) {
        const msg = await res.text();
        throw new Error(msg || 'Failed to install service');
      }

      onDeploySuccess();
      onClose();
    } catch (err: any) {
      setError(err.message || 'Deployment error');
      if (err instanceof BuildError) {
        setBuildLog(err.log);
      }
    } finally {
      setPhase(null);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4 overflow-y-auto">
      <div className="w-full max-w-2xl rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200 my-8">
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
              <Rocket className="h-5 w-5" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground">Deploy Microservice</h3>
              <p className="text-xs text-muted-foreground">Scale-to-zero workload deployment</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Source Tabs */}
        <div className="flex gap-2 border-b border-border my-4 pb-2">
          <button
            type="button"
            onClick={() => setActiveTab('git')}
            className={`inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
              activeTab === 'git'
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'bg-secondary text-secondary-foreground hover:bg-secondary/80'
            }`}
          >
            <FolderGit2 className="h-3.5 w-3.5" />
            <span>GitHub / Git Repo</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab('image')}
            className={`inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
              activeTab === 'image'
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'bg-secondary text-secondary-foreground hover:bg-secondary/80'
            }`}
          >
            <Layers className="h-3.5 w-3.5" />
            <span>Docker Image</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab('dockerfile')}
            className={`inline-flex items-center gap-2 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
              activeTab === 'dockerfile'
                ? 'bg-primary text-primary-foreground font-semibold'
                : 'bg-secondary text-secondary-foreground hover:bg-secondary/80'
            }`}
          >
            <Code className="h-3.5 w-3.5" />
            <span>Dockerfile</span>
          </button>
        </div>

        {error && (
          <div className="mb-4 rounded-lg bg-destructive/10 border border-destructive/20 p-3 text-xs text-destructive-foreground">
            {error}
            {buildLog.length > 0 && (
              <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded bg-background/60 p-2 font-mono text-[10px] text-muted-foreground">
                {buildLog.slice(-40).join('\n')}
              </pre>
            )}
          </div>
        )}

        <form onSubmit={handleSubmit} className="space-y-4 text-xs">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div>
              <label className="block font-medium text-foreground mb-1">
                Service Name <span className="text-red-400">*</span>
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value.toLowerCase().replace(/[^a-z0-9-_]/g, ''))}
                placeholder="e.g. pdf-parser, ai-search"
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
              <p className="mt-1 text-[11px] text-muted-foreground">
                Reachable at <code className="text-blue-400">/v1/{name || '<name>'}/</code>
              </p>
            </div>

            <div>
              <label className="block font-medium text-foreground mb-1">Port</label>
              <input
                type="number"
                value={port}
                onChange={(e) => setPort(Number(e.target.value))}
                placeholder="8080"
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>
          </div>

          {activeTab === 'git' && (
            <div>
              <label className="block font-medium text-foreground mb-1">
                GitHub / Git Repository URL <span className="text-red-400">*</span>
              </label>
              <input
                type="url"
                value={gitUrl}
                onChange={(e) => setGitUrl(e.target.value)}
                placeholder="https://github.com/username/fastapi-service.git"
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>
          )}

          {activeTab === 'image' && (
            <div>
              <label className="block font-medium text-foreground mb-1">
                Docker Image Tag <span className="text-red-400">*</span>
              </label>
              <input
                type="text"
                value={dockerImage}
                onChange={(e) => setDockerImage(e.target.value)}
                placeholder="e.g. python:3.11-slim, redis:7-alpine, myorg/api:v1"
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>
          )}

          {activeTab === 'dockerfile' && (
            <div>
              <label className="block font-medium text-foreground mb-1">Dockerfile Content</label>
              <textarea
                value={dockerfileContent}
                onChange={(e) => setDockerfileContent(e.target.value)}
                rows={5}
                className="w-full rounded-md border border-input bg-background p-2.5 font-mono text-[11px] text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
              <label className="block font-medium text-foreground mt-3 mb-1">
                Build context repo (optional)
              </label>
              <input
                type="url"
                value={contextUrl}
                onChange={(e) => setContextUrl(e.target.value)}
                placeholder="https://github.com/username/fastapi-service.git"
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
              />
              <p className="mt-1 text-[10px] text-muted-foreground">
                Cloned as the build context so COPY has files to copy. Without it the Dockerfile builds on its own.
              </p>
            </div>
          )}

          {/* Scale-to-Zero & Auth Options */}
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 pt-2 border-t border-border">
            <div>
              <label className="block font-medium text-foreground mb-1">
                Cooldown (Seconds before sleep)
              </label>
              <input
                type="number"
                value={cooldown}
                onChange={(e) => setCooldown(Number(e.target.value))}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring"
              />
            </div>

            <div className="flex items-center pt-5">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={isPublic}
                  onChange={(e) => setIsPublic(e.target.checked)}
                  className="rounded border-input text-primary focus:ring-ring h-4 w-4 bg-background"
                />
                <span className="font-medium text-foreground">
                  Public API (No JWT Required)
                </span>
              </label>
            </div>

            <div className="sm:col-span-2">
              <label className="flex items-start gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={localUpstream}
                  onChange={(e) => setLocalUpstream(e.target.checked)}
                  className="mt-0.5 rounded border-input text-primary focus:ring-ring h-4 w-4 bg-background"
                />
                <span>
                  <span className="font-medium text-foreground">Call as a local client</span>
                  <span className="block text-[11px] text-muted-foreground">
                    Drops the browser's Origin and sends Host: localhost. Turn on for apps that only
                    accept local callers, such as MCP servers (Lightpanda: "Origin not allowed").
                  </span>
                </span>
              </label>
            </div>
          </div>

          {/* Environment Variables & Secrets */}
          <div className="pt-2 border-t border-border">
            <div className="flex items-center justify-between mb-2">
              <div>
                <span className="font-medium text-foreground">Environment Variables & Secrets (.env)</span>
                <p className="text-[11px] text-muted-foreground">Injected into the container upon cold start</p>
              </div>
              <button
                type="button"
                onClick={handleAddEnv}
                className="inline-flex items-center gap-1 text-[11px] font-medium text-primary hover:text-primary/80"
              >
                <Plus className="h-3 w-3" />
                <span>Add Variable</span>
              </button>
            </div>

            <div className="space-y-2 max-h-44 overflow-y-auto pr-1">
              {envPairs.map((pair, index) => (
                <div key={index} className="flex items-center gap-2">
                  <input
                    type="text"
                    placeholder="KEY"
                    value={pair.key}
                    onChange={(e) => handleEnvChange(index, 'key', e.target.value)}
                    className="flex-1 rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-[11px] text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                  />
                  <input
                    type={pair.isSecret ? 'password' : 'text'}
                    placeholder="VALUE"
                    value={pair.value}
                    onChange={(e) => handleEnvChange(index, 'value', e.target.value)}
                    className="flex-1 rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-[11px] text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                  />
                  <label className="flex items-center gap-1 cursor-pointer text-[10px] text-muted-foreground px-1" title="Store securely as encrypted secret">
                    <input
                      type="checkbox"
                      checked={pair.isSecret}
                      onChange={(e) => handleEnvChange(index, 'isSecret', e.target.checked)}
                      className="rounded border-input h-3.5 w-3.5 bg-background text-primary"
                    />
                    <Shield className="h-3 w-3 text-amber-400" />
                  </label>
                  <button
                    type="button"
                    onClick={() => handleRemoveEnv(index)}
                    className="p-1 rounded text-muted-foreground hover:text-red-400"
                  >
                    <Trash className="h-3.5 w-3.5" />
                  </button>
                </div>
              ))}
            </div>
          </div>

          {phase === 'building' && (
            <div className="rounded-lg border border-blue-500/20 bg-blue-500/5 p-3 space-y-2">
              <div className="flex items-center justify-between gap-2">
                <div className="flex min-w-0 items-center gap-2 text-foreground">
                  <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-blue-400" />
                  <span className="font-medium">
                    {stopping ? 'Stopping build' : 'Building image'} ·{' '}
                    {formatElapsed(buildStartedAt ? (now - buildStartedAt) / 1000 : 0)}
                  </span>
                </div>
                <span className="text-[10px] text-muted-foreground">Still running; output updates every 2s</span>
              </div>
              {buildStatus?.step && (
                <p className="truncate font-mono text-[10px] text-muted-foreground" title={buildStatus.step}>
                  $ {buildStatus.step}
                </p>
              )}
              <pre
                ref={logRef}
                className="max-h-40 overflow-auto whitespace-pre-wrap rounded bg-background/60 p-2 font-mono text-[10px] text-muted-foreground"
              >
                {buildStatus && buildStatus.log.length > 0
                  ? buildStatus.log.slice(-60).join('\n')
                  : 'Waiting for output...'}
              </pre>
              <p className="text-[10px] text-muted-foreground">
                Large projects can take many minutes. Builds stop after 15 minutes unless the server sets
                AMOEBA_BUILD_TIMEOUT_SECS.
              </p>
            </div>
          )}

          <div className="flex items-center justify-end gap-2 pt-4 border-t border-border">
            {phase === 'building' ? (
              <button
                type="button"
                onClick={handleStopBuild}
                disabled={stopping}
                className="inline-flex items-center gap-1.5 rounded-md px-3.5 py-2 text-xs font-medium text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-50"
              >
                <Square className="h-3 w-3" />
                <span>{stopping ? 'Stopping...' : 'Stop build'}</span>
              </button>
            ) : (
              <button
                type="button"
                onClick={onClose}
                className="rounded-md px-3.5 py-2 text-xs font-medium text-muted-foreground hover:text-foreground hover:bg-secondary transition-colors"
              >
                Cancel
              </button>
            )}
            <button
              type="submit"
              disabled={isSubmitting}
              className="inline-flex items-center gap-1.5 rounded-md bg-primary px-4 py-2 text-xs font-semibold text-primary-foreground shadow-sm hover:bg-primary/90 transition-colors disabled:opacity-50"
            >
              <Rocket className="h-3.5 w-3.5" />
              <span>{phase === 'building'
                  ? 'Building image...'
                  : phase === 'deploying'
                    ? 'Deploying...'
                    : 'Deploy to Amoeba'}</span>
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};

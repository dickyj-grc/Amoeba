import React, { useEffect, useState } from 'react';
import { api } from './api/client';
import { CliInfoModal } from './components/CliInfoModal';
import { DeployModal } from './components/DeployModal';
import { LoginModal } from './components/LoginModal';
import { Navbar } from './components/Navbar';
import { ServicesTable } from './components/ServicesTable';
import { ServiceTokenModal } from './components/ServiceTokenModal';
import { StatsCards } from './components/StatsCards';
import { TestEndpointModal } from './components/TestEndpointModal';
import { ServiceItem } from './types/amoeba';

export const App: React.FC = () => {
  const [isAuthenticated, setIsAuthenticated] = useState<boolean>(!!api.getToken());
  const [services, setServices] = useState<ServiceItem[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(false);
  const [isDeployOpen, setIsDeployOpen] = useState<boolean>(false);
  const [isCliOpen, setIsCliOpen] = useState<boolean>(false);
  const [selectedTestService, setSelectedTestService] = useState<ServiceItem | null>(null);
  const [selectedTokenService, setSelectedTokenService] = useState<string | null>(null);

  const fetchServices = async () => {
    if (!api.getToken()) {
      setIsAuthenticated(false);
      return;
    }
    setIsLoading(true);
    try {
      const data = await api.listApps();
      setServices(data);
      setIsAuthenticated(true);
    } catch (err) {
      // If unauthorized, token is cleared by client
      if (!api.getToken()) {
        setIsAuthenticated(false);
      }
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    const handleUnauthorized = () => setIsAuthenticated(false);
    window.addEventListener('amoeba:unauthorized', handleUnauthorized);

    if (isAuthenticated) {
      fetchServices();
      // Auto-refresh lifecycle state every 3 seconds for scale-to-zero live updates!
      const interval = setInterval(fetchServices, 3000);
      return () => {
        clearInterval(interval);
        window.removeEventListener('amoeba:unauthorized', handleUnauthorized);
      };
    }

    return () => window.removeEventListener('amoeba:unauthorized', handleUnauthorized);
  }, [isAuthenticated]);

  const handleLogout = () => {
    api.clearToken();
    setIsAuthenticated(false);
    setServices([]);
  };

  const handleUninstall = async (name: string) => {
    await api.uninstallApp(name);
    await fetchServices();
  };

  return (
    <div className="min-h-screen bg-background text-foreground flex flex-col selection:bg-primary/20 selection:text-primary">
      <Navbar
        onOpenDeploy={() => setIsDeployOpen(true)}
        onLogout={handleLogout}
        onOpenCli={() => setIsCliOpen(true)}
        isAuthenticated={isAuthenticated}
      />

      <main className="flex-1 max-w-7xl w-full mx-auto px-4 sm:px-6 lg:px-8 py-8">
        <StatsCards services={services} isLoading={isLoading && services.length === 0} />

        <ServicesTable
          services={services}
          isLoading={isLoading}
          onRefresh={fetchServices}
          onUninstall={handleUninstall}
          onTestEndpoint={(s) => setSelectedTestService(s)}
          onIssueToken={(name) => setSelectedTokenService(name)}
        />
      </main>

      <footer className="border-t border-border/60 py-6 text-center text-xs text-muted-foreground">
        <p>Amoeba L7 Compute Gateway • Rust (Axum) & React Control Plane</p>
      </footer>

      {/* Modals */}
      <LoginModal
        isOpen={!isAuthenticated}
        onSuccess={() => {
          setIsAuthenticated(true);
          fetchServices();
        }}
      />

      <DeployModal
        isOpen={isDeployOpen}
        onClose={() => setIsDeployOpen(false)}
        onDeploySuccess={fetchServices}
      />

      <TestEndpointModal
        service={selectedTestService}
        onClose={() => setSelectedTestService(null)}
      />

      <ServiceTokenModal
        serviceName={selectedTokenService}
        onClose={() => setSelectedTokenService(null)}
      />

      <CliInfoModal
        isOpen={isCliOpen}
        onClose={() => setIsCliOpen(false)}
      />
    </div>
  );
};

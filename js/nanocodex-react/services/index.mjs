"use client";
import { createContext, createElement, useContext, useEffect, useRef, useState } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { openHostedPopup } from 'nanocodex/services';

const ServicesContext = createContext(null);
const clientKeys = new WeakMap();
let sequence = 0;
function key(client) { if (!clientKeys.has(client)) clientKeys.set(client, ++sequence); return clientKeys.get(client); }

/** Supply a standalone service client; no agent provider is needed. */
export function ServicesProvider({ client, children }) {
  if (!client) throw new TypeError('ServicesProvider requires a client');
  return createElement(ServicesContext.Provider, { value: client }, children);
}
export function useServices() {
  const client = useContext(ServicesContext);
  if (!client) throw new Error('Service hooks require ServicesProvider');
  return client;
}
export function useVault() {
  const client = useServices();
  return useQuery({ queryKey: ['nanocodex-services', key(client), 'vault'], queryFn: ({ signal }) => client.vault.list({ signal }) });
}
export function usePhoneNumbers() {
  const client = useServices();
  return useQuery({ queryKey: ['nanocodex-services', key(client), 'phone'], queryFn: ({ signal }) => client.phone.list({ signal }) });
}
export function usePhoneMessages(id, query) {
  const client = useServices();
  return useQuery({ queryKey: ['nanocodex-services', key(client), 'messages', id, query], enabled: !!id,
    queryFn: ({ signal }) => client.phone.messages(id, query, { signal }) });
}
export function usePhoneRequest(id) {
  const client = useServices();
  return useQuery({ queryKey: ['nanocodex-services', key(client), 'request', id], enabled: !!id,
    queryFn: ({ signal }) => client.phone.requests.get(id, { signal }) });
}
export function useProvisionPhone() {
  const client = useServices();
  return useMutation({ mutationKey: ['nanocodex-services', key(client), 'provision'], retry: false,
    mutationFn: input => client.phone.provision(input) });
}
export function useReleasePhone() {
  const client = useServices();
  return useMutation({ mutationKey: ['nanocodex-services', key(client), 'release'], retry: false,
    mutationFn: ({ id, ...input }) => client.phone.release(id, input) });
}
export function useVaultRequest() {
  const client = useServices();
  return useMutation({ mutationKey: ['nanocodex-services', key(client), 'vault-request'], retry: false,
    mutationFn: input => client.vault.request(input) });
}

/** Opens the account's top-level private form directly from a user click. */
export function HostedServiceButton({ request, onComplete, onError, children, className, style }) {
  const active = useRef(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(null);
  useEffect(() => {
    setPending(false);
    setError(null);
    return () => { active.current?.abort(); active.current = null; };
  }, [request]);
  const open = async () => {
    if (active.current) return;
    const controller = new AbortController();
    active.current = controller;
    setPending(true);
    setError(null);
    try {
      const result = await openHostedPopup(request, { signal: controller.signal });
      if (!controller.signal.aborted) onComplete?.(result);
    } catch (cause) {
      if (!controller.signal.aborted) { setError(cause instanceof Error ? cause : new Error('Hosted enrollment failed')); onError?.(cause); }
    } finally {
      if (active.current === controller) { active.current = null; setPending(false); }
    }
  };
  return createElement('div', { className, style },
    createElement('button', { type: 'button', disabled: pending, onClick: open },
      pending ? 'Waiting for account approval…' : children ?? (request.service === 'phone' ? 'Review phone request' : request.action === 'select' ? 'Choose a Vault item' : 'Set up authenticator')),
    error ? createElement('p', { role: 'alert' }, error.message) : null);
}

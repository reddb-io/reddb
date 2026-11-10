<?php
declare(strict_types=1);
namespace Reddb;

use Reddb\RedDBException\AuthRefused;
use Reddb\RedDBException\NetworkError;

final class RoutedConn implements Conn
{
    private array $clients;
    private array $replicas = [];
    private string $primary;
    private ?string $pinned = null;
    private bool $session = false;
    private bool $broken = false;
    private bool $closed = false;
    private int $refreshAt = 0;
    private int $next = 0;

    public function __construct(private readonly string $seed, Conn $initial, private readonly string $protocol,
        private readonly bool $forcePrimary, private readonly \Closure $dial)
    {
        $this->primary = $seed; $this->clients = [$seed => $initial];
    }
    private function endpoint(string $address): Conn
    {
        if (isset($this->clients[$address])) return $this->clients[$address];
        $client = ($this->dial)($address, false);
        try { $client->ping(); }
        catch (\Throwable $error) { $client->close(); throw $error; }
        return $this->clients[$address] = $client;
    }
    public function refresh(): void
    {
        if ($this->pinned !== null || $this->closed) return;
        $this->refreshAt = hrtime(true) + 30_000_000_000;
        $deadline = hrtime(true) + 5_000_000_000;
        $candidates = array_values(array_unique([$this->primary, $this->seed, ...array_column($this->replicas, 'addr')]));
        for ($index = 0; $index < count($candidates) && $index < 258 && hrtime(true) < $deadline; $index++) {
            $control = null;
            try {
                $control = ($this->dial)($candidates[$index], true);
                if (!$control instanceof TopologySource) continue;
                $topology = $control->topology($this->protocol);
                if (($topology['version'] ?? null) !== 1) continue;
                $primary = Topology::address($topology['primary']['addr'] ?? null, $this->protocol);
                if ($primary === null) continue;
                if (!in_array($primary, $candidates, true)) $candidates[] = $primary;
                $this->endpoint($primary); $ready = [];
                foreach (array_slice($topology['replicas'] ?? [], 0, 256) as $replica) {
                    if (hrtime(true) >= $deadline) break;
                    $address = Topology::address($replica['addr'] ?? null, $this->protocol);
                    if ($address === null || $address === $primary || ($replica['healthy'] ?? false) !== true || ($replica['rebootstrapping'] ?? false)) continue;
                    try { $this->endpoint($address); $ready[] = [...$replica, 'addr' => $address]; }
                    catch (\RuntimeException $error) { }
                }
                $this->primary = $primary; $this->replicas = $ready;
                $keep = [$this->seed, $primary, ...array_column($ready, 'addr')];
                foreach ($this->clients as $address => $client) {
                    if (!in_array($address, $keep, true)) { $client->close(); unset($this->clients[$address]); }
                }
                if ($primary === $candidates[$index]) return;
            } catch (AuthRefused $error) { throw $error; }
            catch (\RuntimeException $error) { }
            finally { $control?->close(); }
        }
    }
    private function call(\Closure $operation, ?string $sql = null, bool $read = false, ?ReadOptions $options = null): mixed
    {
        if ($this->closed || $this->broken) throw new \LogicException('connection is closed');
        if ($this->pinned === null && hrtime(true) >= $this->refreshAt) $this->refresh();
        [$state, $safe] = $sql === null ? ['primary', false] : RoutingSql::policy($sql);
        $read = $read || ($safe && $options !== null);
        $eligible = $this->forcePrimary || $options?->consistency === 'strong' ? []
            : array_values(array_filter($this->replicas, static fn(array $r): bool =>
                $options?->consistency !== 'bounded_staleness' || ($r['lag_ms'] ?? PHP_INT_MAX) <= $options->maxLagMs));
        $address = $this->pinned ?? ($read && $eligible ? $eligible[$this->next++ % count($eligible)]['addr'] : $this->primary);
        if ($state === 'begin' || $state === 'session') { $this->pinned = $address; $this->session = $this->session || $state === 'session'; }
        try {
            $result = $operation($this->endpoint($address));
            if ($state === 'end' && !$this->session) $this->pinned = null;
            return $result;
        } catch (NetworkError $error) {
            $this->refreshAt = 0;
            if (isset($this->clients[$address])) { $this->clients[$address]->close(); unset($this->clients[$address]); }
            if ($this->pinned !== null) $this->broken = true;
            if ($this->pinned !== null || !$read) throw $error;
            $this->refresh(); return $operation($this->endpoint($this->primary));
        }
    }
    public function query(string $sql, array $params = []): string { return $this->call(fn(Conn $c) => $c->query($sql, $params), $sql); }
    public function queryWithOptions(string $sql, ReadOptions $options, array $params = []): string { return $this->call(fn(Conn $c) => $c->query($sql, $params), $sql, options: $options); }
    public function insert(string $collection, array|object $payload): void { $this->call(fn(Conn $c) => $c->insert($collection, $payload)); }
    public function bulkInsert(string $collection, iterable $rows): void { $this->call(fn(Conn $c) => $c->bulkInsert($collection, $rows)); }
    public function get(string $collection, string $id): string { return $this->call(fn(Conn $c) => $c->get($collection, $id), read: true); }
    public function delete(string $collection, string $id): void { $this->call(fn(Conn $c) => $c->delete($collection, $id)); }
    public function ping(): void { $this->call(fn(Conn $c) => $c->ping()); }
    public function close(): void { $this->closed = true; foreach ($this->clients as $client) $client->close(); $this->clients = []; }
}

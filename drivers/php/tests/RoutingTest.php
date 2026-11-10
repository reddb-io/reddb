<?php
declare(strict_types=1);
namespace Reddb\Tests;
use PHPUnit\Framework\TestCase;
use Reddb\{Conn, TopologySource, Topology, RoutingSql, RoutedConn, ReadOptions};
use Reddb\RedDBException\NetworkError;

final class RoutingTest extends TestCase {
    public function testSharedFixtures(): void {
        $root = dirname(__DIR__, 3) . '/testdata/conformance/topology/';
        foreach (json_decode(file_get_contents($root . 'manifest.json'), true)['cases'] as $case) {
            try {
                $actual = Topology::decode(hex2bin($case['hex']));
                self::assertFalse($case['error'] ?? false, $case['name']);
                self::assertEquals($case['expected'], $actual, $case['name']);
            } catch (\Reddb\RedDBException\ProtocolError $error) {
                self::assertTrue($case['error'] ?? false, $case['name']);
            }
        }
        foreach (json_decode(file_get_contents($root . 'sql-policy.json'), true) as $case)
            self::assertSame($case['replica_read'], RoutingSql::policy($case['sql'])[1], $case['sql']);
    }
    public function testRoutesReadsAndNeverReplaysEffects(): void {
        $calls = []; $primary = 'https://primary:5000'; $replica = 'https://replica:5000';
        $membership = ['version'=>1, 'primary'=>['addr'=>$primary], 'replicas'=>[['addr'=>$replica,'healthy'=>true,'lag_ms'=>10]]];
        $dial = function(string $address, bool $control) use (&$calls,$membership): Conn {
            return new class($address,$calls,$membership) implements Conn, TopologySource {
                public function __construct(private string $address, public array &$calls, private array $membership) {}
                public function topology(string $protocol): ?array { return $this->membership; }
                public function query(string $sql, array $params=[]): string {
                    $this->calls[] = [$this->address,$sql];
                    if (str_contains($sql,'lost') || str_contains($sql,'effectful')) throw new NetworkError('lost reply');
                    return $this->address;
                }
                public function insert(string $c,array|object $p):void {}
                public function bulkInsert(string $c,iterable $p):void {}
                public function get(string $c,string $id):string {return $this->query('get');}
                public function delete(string $c,string $id):void {}
                public function ping():void {} public function close():void {}
            };
        };
        $client = new RoutedConn($primary,$dial($primary,false),'https',false,$dial);
        self::assertSame($primary,$client->query('SELECT 1'));
        self::assertSame($replica,$client->queryWithOptions('SELECT 1',new ReadOptions('local')));
        $client->query('BEGIN');
        self::assertSame($primary,$client->queryWithOptions('SELECT 1',new ReadOptions('local')));
        $client->query('ROLLBACK');
        foreach (['UPDATE lost','SELECT "effectful"()'] as $sql) {
            $before = count($calls);
            try { $client->queryWithOptions($sql,new ReadOptions('local')); self::fail('expected failure'); }
            catch (NetworkError $error) { self::assertCount($before+1,$calls); self::assertSame($primary,$calls[array_key_last($calls)][0]); }
        }
        $client->close();
    }
}

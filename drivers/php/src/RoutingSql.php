<?php
declare(strict_types=1);
namespace Reddb;

final class RoutingSql
{
    public static function policy(string $sql): array
    {
        $tokens = self::tokens($sql);
        if (!$tokens || in_array(';', $tokens, true)) return ['session', false];
        $first = $tokens[0];
        if ($first === 'BEGIN' || ($first === 'START' && ($tokens[1] ?? '') === 'TRANSACTION')) return ['begin', false];
        if (in_array($first, ['COMMIT', 'END', 'ROLLBACK'], true)) {
            if ($first === 'ROLLBACK' && in_array('TO', $tokens, true)) return ['primary', false];
            return [in_array('CHAIN', $tokens, true) && !in_array('NO', $tokens, true) ? 'begin' : 'end', false];
        }
        if (in_array($first, ['SET', 'RESET', 'USE', 'PREPARE', 'DEALLOCATE', 'DECLARE', 'LISTEN'], true)) return ['session', false];
        $forbidden = explode(' ', 'INSERT UPDATE DELETE CREATE ALTER DROP INTO FOR LOCK CALL EXECUTE SET QUEUE COPY [ ]');
        $functions = explode(' ', 'COUNT SUM MIN MAX AVG LOWER UPPER LENGTH COALESCE ABS ROUND CAST EXISTS IN AS FROM WHERE AND OR NOT SELECT');
        $read = $first === 'SELECT' && !array_intersect($tokens, $forbidden);
        foreach ($tokens as $index => $token) {
            if (($tokens[$index + 1] ?? '') === '(' && ($token === '#identifier' || ctype_alpha($token[0]) || $token[0] === '_')
                && !in_array($token, $functions, true)) $read = false;
        }
        return ['primary', $read];
    }

    private static function tokens(string $sql): ?array
    {
        $length = strlen($sql);
        if ($length > 1_048_576) return null;
        $tokens = []; $index = 0;
        while ($index < $length) {
            $char = $sql[$index];
            if (ctype_space($char)) { $index++; continue; }
            if (substr($sql, $index, 2) === '--') {
                $end = strpos($sql, "\n", $index + 2); $index = $end === false ? $length : $end + 1; continue;
            }
            if (substr($sql, $index, 2) === '/*') {
                $depth = 1; $index += 2;
                while ($index < $length && $depth > 0) {
                    if (substr($sql, $index, 2) === '/*') { $depth++; $index += 2; }
                    elseif (substr($sql, $index, 2) === '*/') { $depth--; $index += 2; }
                    else $index++;
                    if ($depth > 64) return null;
                }
                if ($depth !== 0) return null;
                continue;
            }
            if ($char === "'" || $char === '"' || $char === '`') {
                $index++; $closed = false;
                while ($index < $length) {
                    $value = $sql[$index++];
                    if ($value === '\\') return null;
                    if ($value === $char) {
                        if ($index < $length && $sql[$index] === $char) { $index++; continue; }
                        $closed = true; break;
                    }
                }
                if (!$closed) return null;
                $tokens[] = $char === "'" ? '#value' : '#identifier'; continue;
            }
            if (ord($char) > 127) return null;
            $start = $index++;
            if (ctype_alpha($char) || $char === '_')
                while ($index < $length && (ctype_alnum($sql[$index]) || $sql[$index] === '_')) $index++;
            $tokens[] = strtoupper(substr($sql, $start, $index - $start));
        }
        if ($tokens && end($tokens) === ';') array_pop($tokens);
        return $tokens;
    }
}

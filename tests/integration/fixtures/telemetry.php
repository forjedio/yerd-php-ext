<?php
// Integration fixture: exercises every observed category WITHOUT requiring a
// full Laravel/Composer install. It declares minimal stand-ins for the symbols
// the extension observes (Symfony VarDumper, the Laravel event classes) in
// separate-from-main-op_array form, then drives them.
//
// Real CI also runs against a real Laravel app; this fixture keeps the smoke
// test hermetic and fast.

namespace Symfony\Component\VarDumper {
    class VarDumper
    {
        public static function dump($var) { return $var; }
    }
}

namespace Illuminate\Events {
    class Dispatcher
    {
        public function dispatch($event, $payload = [], $halt = false) { return null; }
    }
}

namespace Illuminate\Cache\Events {
    class CacheHit { public $key = 'user:1'; public $storeName = 'redis'; }
    class KeyWritten { public $key = 'config'; public $storeName = 'file'; }
}

namespace Illuminate\Queue\Events {
    class JobProcessing
    {
        public $connectionName = 'redis';
        public $queue = 'default';
        public $job = 'App\\Jobs\\SendWelcomeEmail';
    }
}

namespace Illuminate\Log\Events {
    class MessageLogged
    {
        public $level = 'warning';
        public $message = 'disk almost full';
        public $context = ['free' => '5%'];
    }
}

namespace {
    use Symfony\Component\VarDumper\VarDumper;

    // dumps (via the VarDumper chokepoint, as Laravel's dump()/dd() funnel through)
    VarDumper::dump(['hello', 42, true, ['nested' => 1]]);
    VarDumper::dump('a plain string');

    // queries (framework-agnostic, real PDO)
    $pdo = new PDO('sqlite::memory:');
    $pdo->exec('CREATE TABLE users (id INTEGER, name TEXT)');
    $stmt = $pdo->prepare('INSERT INTO users (id, name) VALUES (?, ?)');
    $stmt->execute([1, 'Ada']);
    $pdo->query('SELECT * FROM users');

    // Laravel signals (jobs / cache / logs / views) via the dispatcher
    $d = new Illuminate\Events\Dispatcher();
    $d->dispatch(new Illuminate\Cache\Events\CacheHit());
    $d->dispatch(new Illuminate\Cache\Events\KeyWritten());
    $d->dispatch(new Illuminate\Queue\Events\JobProcessing());
    $d->dispatch(new Illuminate\Log\Events\MessageLogged());
    $d->dispatch('composing: profile.show', []);

    // outgoing HTTP (only when the smoke provides a local target + curl exists)
    $http_url = getenv('YERD_TEST_HTTP_URL');
    if ($http_url && function_exists('curl_init')) {
        $ch = curl_init($http_url);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_TIMEOUT, 5);
        curl_exec($ch);
    }

    echo "fixture-complete\n";
}

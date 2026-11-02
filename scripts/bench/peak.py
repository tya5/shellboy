"""Periodic, explicitly incomplete observations of process-tree PSS."""
import threading
import time


class PeakSampler:
    def __init__(self, sample, interval=0.1):
        if interval <= 0:
            raise ValueError('sampling interval must be positive')
        self.sample = sample
        self.interval = interval
        self.stop_event = threading.Event()
        self.samples = []
        self.churn_failures = 0
        self.error = None
        self.started = None
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self.stop_event.is_set():
            start = time.monotonic_ns()
            try:
                row = self.sample()
                end = time.monotonic_ns()
                self.samples.append({'start_ms': (start - self.started) / 1e6,
                                     'end_ms': (end - self.started) / 1e6, **row})
            except RuntimeError as error:
                if 'Process tree changed' in str(error) or 'Process trees changed' in str(error):
                    self.churn_failures += 1
                else:
                    self.error = f'{type(error).__name__}: {error}'
                    return
            except Exception as error:
                self.error = f'{type(error).__name__}: {error}'
                return
            self.stop_event.wait(max(0, self.interval - (time.monotonic_ns() - start) / 1e9))

    def __enter__(self):
        self.started = time.monotonic_ns()
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop_event.set()
        self.thread.join()

    def result(self):
        if self.error:
            raise RuntimeError(f'PSS observer failed: {self.error}')
        values = [row['pss_kib'] for row in self.samples if row.get('pss_kib') is not None]
        return {'interval_ms': self.interval * 1000, 'samples': self.samples,
                'churn_failures': self.churn_failures,
                'observed_max_pss_kib': max(values) if values else None}

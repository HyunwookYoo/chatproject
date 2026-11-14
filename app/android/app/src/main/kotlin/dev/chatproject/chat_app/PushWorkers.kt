package dev.chatproject.chat_app

import android.content.Context
import android.os.Build
import android.util.Log
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import uniffi.chat_ffi.nativePushRenew

// Push work without the app on screen. Neither runs as a foreground service, so an everyday
// phone shows no notification of its own for it.
object PushWorkers {
    const val RENEW = "push_renew"
    const val FETCH = "push_fetch"
    // FETCH_MAX_MS (chat_core::push): the longest a wake's fetch keeps the network on.
    const val FETCH_DEADLINE_MS = 20_000
    private const val RENEW_HOURS = 24L

    private val network = Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()

    // The handle renewal (spec 5.1), every 24 h. The first run waits a period too: while the
    // app runs, its core checks the handles itself every hour.
    fun scheduleRenewal(context: Context) {
        val request = PeriodicWorkRequestBuilder<PushRenewWorker>(RENEW_HOURS, TimeUnit.HOURS)
            .setConstraints(network)
            .setInitialDelay(RENEW_HOURS, TimeUnit.HOURS)
            .build()
        WorkManager.getInstance(context).enqueueUniquePeriodicWork(RENEW, ExistingPeriodicWorkPolicy.KEEP, request)
    }

    // The rest of a wake push's fetch after FCM's processing time. Expedited from Android 12,
    // where it runs as an expedited job. Before that WorkManager would run expedited work as a
    // foreground service with a notification, so there it is ordinary work. A fetch already
    // pending or running is kept: it reaches the same sender and my devices.
    fun scheduleFetch(context: Context, data: Map<String, String>) {
        val builder = OneTimeWorkRequestBuilder<PushFetchWorker>()
            .setConstraints(network)
            .setInputData(Data.Builder().putAll(data).build())
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            builder.setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
        }
        WorkManager.getInstance(context).enqueueUniqueWork(FETCH, ExistingWorkPolicy.KEEP, builder.build())
    }
}

class PushRenewWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        try {
            NativeCore.ensureStarted(applicationContext)
            nativePushRenew()
        } catch (error: Exception) {
            // The next period tries again; handles stay valid for 30 days.
            Log.w(ChatApplication.LOG_TAG, "Push renewal not done: ${error.javaClass.simpleName}")
        }
        return Result.success()
    }
}

class PushFetchWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val data = inputData.keyValueMap.mapNotNull { (key, value) -> (value as? String)?.let { key to it } }.toMap()
        PushService.fetch(applicationContext, data)
        return Result.success()
    }
}
